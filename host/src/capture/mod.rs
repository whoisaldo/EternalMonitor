//! Capture stage: produces BGRA `RawFrame`s for the encoder.
//!
//! The platform backend is DXGI Desktop Duplication on Windows ([`dxgi`]) and
//! a deterministic test pattern everywhere else ([`synthetic`], also
//! selectable on Windows with `ETERNAL_CAPTURE=synthetic`). Everything in this
//! file is portable policy: output selection, pacing, and the virtual-display
//! reconciliation that decides *what* to capture.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use tracing::error;
#[cfg(windows)]
use tracing::{info, warn};

use crate::control::SharedControl;
#[cfg(windows)]
use crate::control::{CaptureTarget, VddStatus};

#[cfg(windows)]
pub mod dxgi;
pub mod synthetic;

#[cfg(windows)]
pub(crate) const ACQUIRE_TIMEOUT_MS: u32 = 16;
#[cfg(windows)]
pub(crate) const FPS_WINDOW: usize = 60;

/// When the captured display is static (or a freshly-enabled extended display is still blank),
/// DXGI delivers no new frames. If a client is connected and we haven't sent anything for this
/// long, resend the last frame so the encoder emits the pending IDR and the iPad gets a sync
/// sample instead of timing out on a black screen.
#[cfg(windows)]
pub(crate) const IDLE_KEEPALIVE: Duration = Duration::from_millis(750);

pub(crate) fn frame_budget_for(target_fps: u32) -> Duration {
    let fps = target_fps.max(1) as u64;
    Duration::from_micros(1_000_000 / fps)
}

/// Frame data sent downstream for encoding/transport. The pixels are shared
/// (`Arc`) so keepalive resends and the capture loop's buffer reuse never copy
/// a full frame again — the staging-texture readback is the only full-frame
/// copy left on the capture side.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub frame_number: u64,
    pub timestamp: Instant,
    pub data: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// Latest-wins handoff from capture to encoder: a 1-deep mailbox. Publishing
/// displaces an unconsumed older frame, so the encoder always works on the
/// freshest picture instead of draining a backlog (the old 4-deep channel
/// could queue ~67ms of latency under load). Encoded output downstream still
/// uses a lossless channel — dropping an ENCODED P-frame corrupts the GOP,
/// dropping an unencoded capture frame just skips a tick.
pub struct FrameSlot {
    inner: Arc<SlotInner>,
}

struct SlotInner {
    state: Mutex<SlotState>,
    condvar: Condvar,
}

#[derive(Default)]
struct SlotState {
    frame: Option<RawFrame>,
    closed: bool,
}

impl FrameSlot {
    fn new() -> Self {
        Self {
            inner: Arc::new(SlotInner {
                state: Mutex::new(SlotState::default()),
                condvar: Condvar::new(),
            }),
        }
    }

    fn handle(&self) -> FrameSlot {
        FrameSlot {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn publish(&self, frame: RawFrame) {
        let mut state = self.inner.state.lock();
        state.frame = Some(frame); // displaces any unconsumed frame
        self.inner.condvar.notify_one();
    }

    pub fn close(&self) {
        let mut state = self.inner.state.lock();
        state.closed = true;
        self.inner.condvar.notify_all();
    }

    /// Blocks until a frame is available or the producer closed the slot.
    pub fn blocking_take(&self) -> Option<RawFrame> {
        let mut state = self.inner.state.lock();
        loop {
            if let Some(frame) = state.frame.take() {
                return Some(frame);
            }
            if state.closed {
                return None;
            }
            self.inner.condvar.wait(&mut state);
        }
    }
}

/// Stamp a watchdog heartbeat (ms on the process clock).
pub(crate) fn heartbeat(atomic: &std::sync::atomic::AtomicU64) {
    atomic.store(
        crate::clock::host_now_us() / 1000,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// A desktop-attached display output discovered via DXGI, usable as a capture source.
#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub adapter_index: u32,
    pub output_index: u32,
    pub device_name: String,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub is_primary: bool,
    pub adapter_name: String,
}

/// Enumerate every desktop-attached display output. Windows-only in substance;
/// other platforms have no selectable outputs (the synthetic source ignores them).
pub fn enumerate_outputs() -> Vec<OutputInfo> {
    #[cfg(windows)]
    {
        dxgi::enumerate_outputs()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Resolve which output (index into `outputs`) the capture loop should duplicate for
/// `target`. `PrimaryAuto` picks the (0,0) output, else the first attached. `Output(name)`
/// matches the `DeviceName`, falling back to the primary (with a warning) when not found —
/// e.g. the virtual display driver was disabled. Returns `None` only when `outputs` is empty.
#[cfg(any(windows, test))]
pub(crate) fn resolve_target(
    outputs: &[OutputInfo],
    target: &crate::control::CaptureTarget,
) -> Option<usize> {
    use crate::control::CaptureTarget;

    if outputs.is_empty() {
        return None;
    }
    let primary = outputs.iter().position(|o| o.is_primary).unwrap_or(0);
    match target {
        // VirtualExtended is reconciled into an Output(name) before resolution; treat any
        // leftover as primary defensively.
        CaptureTarget::PrimaryAuto | CaptureTarget::VirtualExtended => Some(primary),
        CaptureTarget::Output(name) => match outputs.iter().position(|o| &o.device_name == name) {
            Some(i) => Some(i),
            None => {
                tracing::warn!(
                    requested = %name,
                    "Requested capture display not found (driver disabled/removed?) — \
                     falling back to the primary output"
                );
                Some(primary)
            }
        },
    }
}

/// How long to wait for the virtual display to attach after enabling its driver. Overridable
/// via `ETERNAL_VDD_TIMEOUT_SECS` for machines whose driver loads slowly.
#[cfg(windows)]
const VDD_ATTACH_TIMEOUT_DEFAULT_SECS: u64 = 10;
#[cfg(windows)]
const VDD_POLL_INTERVAL: Duration = Duration::from_millis(120);

#[cfg(windows)]
fn vdd_attach_timeout() -> Duration {
    std::env::var("ETERNAL_VDD_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(VDD_ATTACH_TIMEOUT_DEFAULT_SECS))
}

/// Is an iPad currently registered as a receiver? The virtual display is brought up only while
/// a client is connected, so an idle PC never shows a phantom second monitor.
#[cfg(windows)]
pub(crate) fn client_connected(shared: &SharedControl) -> bool {
    shared.client_connected()
}

/// Reconcile the virtual display device to the requested target and return the concrete
/// target to capture. `VirtualExtended` enables the bundled driver **only once an iPad is
/// connected**, waits for a genuinely-new output to attach, and returns it as an `Output(name)`;
/// any other target (and the no-client / failure / timeout cases) disables the driver so no
/// phantom monitor lingers, falling back to `PrimaryAuto`.
#[cfg(windows)]
pub(crate) fn reconcile_virtual_display(
    target: &CaptureTarget,
    shared: &SharedControl,
) -> CaptureTarget {
    match target {
        CaptureTarget::VirtualExtended => {
            // Defer enabling the VDD until an iPad actually connects. The transport restarts the
            // pipeline on the first receiver registration, so this runs again with a connected
            // client at that point and the display comes up then — never while the PC sits idle.
            if !client_connected(shared) {
                info!(
                    "Extended display selected but no iPad has connected yet — leaving the \
                     virtual display off and mirroring the primary display until a client registers"
                );
                crate::vdd::disable();
                *shared.vdd_status.lock() = VddStatus::WaitingForClient;
                return CaptureTarget::PrimaryAuto;
            }

            // Best effort: match the virtual display's mode list to the
            // connected iPad's panel BEFORE enabling the driver (it reads
            // vdd_settings.xml at enable time). Failure is non-fatal — the
            // driver's built-in modes apply. NEEDS_WINDOWS_VERIFY (M10).
            if crate::settings::SettingsFile::load().vdd_match_resolution {
                if let Some(info) = shared.session.lock().client_info() {
                    let modes = crate::vdd_settings::modes_for_client(
                        info.screen_px.0,
                        info.screen_px.1,
                        info.refresh_hz,
                    );
                    let path = std::path::Path::new(r"C:\VirtualDisplayDriver\vdd_settings.xml");
                    match crate::vdd_settings::write_to(path, &modes) {
                        Ok(()) => info!(
                            ?modes,
                            "Wrote virtual display mode list for the connected client"
                        ),
                        Err(error) => warn!(
                            %error,
                            "Could not write vdd_settings.xml — driver default modes apply"
                        ),
                    }
                }
            }

            let before: Vec<String> = enumerate_outputs()
                .into_iter()
                .map(|o| o.device_name)
                .collect();
            if !crate::vdd::enable() {
                warn!(
                    "Virtual display could not be enabled (installer task missing?) — \
                     capturing the primary display instead"
                );
                *shared.vdd_status.lock() = VddStatus::Failed;
                return CaptureTarget::PrimaryAuto;
            }
            let deadline = Instant::now() + vdd_attach_timeout();
            while Instant::now() < deadline {
                std::thread::sleep(VDD_POLL_INTERVAL);
                let now = enumerate_outputs();
                // Accept ONLY a genuinely new non-primary output (the freshly-enabled VDD). Never
                // grab a pre-existing real second monitor that was attached before we enabled it.
                if let Some(chosen) = pick_new_virtual_output(&before, &now) {
                    info!(
                        device = %chosen.device_name,
                        width = chosen.width,
                        height = chosen.height,
                        "Virtual display attached — capturing it"
                    );
                    *shared.vdd_status.lock() = VddStatus::Active;
                    return CaptureTarget::Output(chosen.device_name.clone());
                }
            }
            warn!("Virtual display did not attach in time — capturing the primary display instead");
            // enable() succeeded but nothing attached: turn it back off so we don't strand a
            // half-enabled device as a phantom monitor.
            crate::vdd::disable();
            *shared.vdd_status.lock() = VddStatus::Failed;
            CaptureTarget::PrimaryAuto
        }
        other => {
            // Any non-virtual target: ensure the virtual display is off.
            crate::vdd::disable();
            *shared.vdd_status.lock() = VddStatus::Inactive;
            other.clone()
        }
    }
}

/// Runs the platform capture loop on the CURRENT thread until stop/close,
/// publishing frames into `slot`. The supervisor spawns and monitors this.
pub fn run_capture_stage(
    slot: FrameSlot,
    shared: SharedControl,
    adapter_index: u32,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    struct CloseOnExit(FrameSlot);
    impl Drop for CloseOnExit {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let guard = CloseOnExit(slot.handle());
    let result = run_platform_loop(&guard.0, shared, adapter_index, generation);
    if let Err(ref e) = result {
        error!(error = %e, "Capture loop exited with error");
    }
    result
}

/// A producer/consumer pair for the capture→encoder handoff.
pub fn frame_slot() -> (FrameSlot, FrameSlot) {
    let producer = FrameSlot::new();
    let consumer = producer.handle();
    (producer, consumer)
}

#[cfg(windows)]
fn run_platform_loop(
    slot: &FrameSlot,
    shared: SharedControl,
    adapter_index: u32,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let synthetic_requested =
        std::env::var("ETERNAL_CAPTURE").is_ok_and(|v| v.trim().eq_ignore_ascii_case("synthetic"));
    if synthetic_requested {
        synthetic::run_capture_loop(slot, shared, generation)
    } else {
        dxgi::run_capture_loop(slot, shared, adapter_index, generation)
    }
}

#[cfg(not(windows))]
fn run_platform_loop(
    slot: &FrameSlot,
    shared: SharedControl,
    _adapter_index: u32,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    synthetic::run_capture_loop(slot, shared, generation)
}

/// Choose the freshly-attached virtual output: the first non-primary output whose device name was
/// not present in `before` (the snapshot taken just before enabling the VDD). Returns `None` when
/// no new output has appeared yet, so the caller keeps polling instead of grabbing a pre-existing
/// real second monitor.
#[cfg(any(windows, test))]
fn pick_new_virtual_output<'a>(before: &[String], now: &'a [OutputInfo]) -> Option<&'a OutputInfo> {
    now.iter()
        .filter(|o| !o.is_primary)
        .find(|o| !before.contains(&o.device_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::CaptureTarget;

    fn out(name: &str, left: i32, top: i32) -> OutputInfo {
        OutputInfo {
            adapter_index: 0,
            output_index: 0,
            device_name: name.to_string(),
            width: 1920,
            height: 1080,
            left,
            top,
            is_primary: left == 0 && top == 0,
            adapter_name: "Test GPU".to_string(),
        }
    }

    #[test]
    fn primary_auto_picks_origin_output() {
        let outputs = vec![out(r"\\.\DISPLAY2", 2560, 0), out(r"\\.\DISPLAY1", 0, 0)];
        let idx = resolve_target(&outputs, &CaptureTarget::PrimaryAuto).unwrap();
        assert_eq!(outputs[idx].device_name, r"\\.\DISPLAY1");
    }

    #[test]
    fn known_output_is_selected() {
        let outputs = vec![out(r"\\.\DISPLAY1", 0, 0), out(r"\\.\DISPLAY3", 2560, 0)];
        let target = CaptureTarget::Output(r"\\.\DISPLAY3".to_string());
        let idx = resolve_target(&outputs, &target).unwrap();
        assert_eq!(outputs[idx].device_name, r"\\.\DISPLAY3");
    }

    #[test]
    fn unknown_output_falls_back_to_primary() {
        let outputs = vec![out(r"\\.\DISPLAY1", 0, 0), out(r"\\.\DISPLAY2", 2560, 0)];
        let target = CaptureTarget::Output(r"\\.\DISPLAY9".to_string());
        let idx = resolve_target(&outputs, &target).unwrap();
        assert!(outputs[idx].is_primary);
    }

    #[test]
    fn empty_outputs_returns_none() {
        assert!(resolve_target(&[], &CaptureTarget::PrimaryAuto).is_none());
    }

    #[test]
    fn picks_only_a_genuinely_new_non_primary_output() {
        // A real second monitor existed before we enabled the VDD; it must NOT be chosen.
        let before = vec![r"\\.\DISPLAY1".to_string(), r"\\.\DISPLAY2".to_string()];
        let now = vec![
            out(r"\\.\DISPLAY1", 0, 0),
            out(r"\\.\DISPLAY2", 2560, 0),
            out(r"\\.\DISPLAY3", -1920, 0), // freshly-attached VDD
        ];
        let chosen = pick_new_virtual_output(&before, &now).expect("new output");
        assert_eq!(chosen.device_name, r"\\.\DISPLAY3");
    }

    #[test]
    fn returns_none_when_only_preexisting_second_monitor_present() {
        // The VDD hasn't attached yet — must keep polling, never grab the existing DISPLAY2.
        let before = vec![r"\\.\DISPLAY1".to_string(), r"\\.\DISPLAY2".to_string()];
        let now = vec![out(r"\\.\DISPLAY1", 0, 0), out(r"\\.\DISPLAY2", 2560, 0)];
        assert!(pick_new_virtual_output(&before, &now).is_none());
    }

    #[test]
    fn ignores_a_new_primary_output() {
        // Even if a new output appears, the primary (origin) one is never the virtual display.
        let before: Vec<String> = vec![];
        let now = vec![out(r"\\.\DISPLAY1", 0, 0)];
        assert!(pick_new_virtual_output(&before, &now).is_none());
    }
}
