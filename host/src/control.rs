use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

use parking_lot::Mutex;
use tracing::warn;

pub const DEFAULT_TARGET_FPS: u32 = 60;

#[derive(Clone)]
pub struct SharedControl {
    pub running: Arc<AtomicBool>,
    /// The user's MAXIMUM bitrate (the GUI slider) — the ABR ceiling.
    pub bitrate_bps: Arc<AtomicU32>,
    /// What the encoder should currently produce: the ABR controller's pick,
    /// always <= `bitrate_bps`. The encoder reopens its session when this
    /// changes (hardware encoders ignore bitrate pokes on an open context).
    pub abr_current_bps: Arc<AtomicU32>,
    pub target_fps: Arc<AtomicU32>,
    pub target_addr: Arc<Mutex<SocketAddr>>,
    /// Set by transport on iPad re-handshake (same target). Encoder
    /// swaps it back to false on the next frame and forces an IDR. AMD only — NVENC
    /// ignores it because NVENC keyframe cadence is already correct.
    pub force_next_idr: Arc<AtomicBool>,
    /// Encoder name override from the Settings tab, applied on next pipeline restart.
    /// `None` means use auto-detected encoder from GpuInfo.
    pub encoder_override: Arc<Mutex<Option<String>>>,
    /// Which display output the capture loop should duplicate, applied on next pipeline
    /// restart. `PrimaryAuto` mirrors the primary monitor (today's behavior).
    pub capture_target: Arc<Mutex<CaptureTarget>>,
    /// Live status of the managed virtual extended display, surfaced in the GUI so a tester
    /// can see when an Extended-display request silently fell back to the primary monitor.
    pub vdd_status: Arc<Mutex<VddStatus>>,
    /// Watchdog heartbeats (ms on the process clock, relaxed): the capture
    /// loop's aliveness, the last produced frame, and the last encoded frame.
    /// The supervisor detects wedged/starved stages from these.
    pub hb_capture_loop_ms: Arc<AtomicU64>,
    pub hb_capture_frame_ms: Arc<AtomicU64>,
    pub hb_encode_frame_ms: Arc<AtomicU64>,
    /// Desktop-space rectangle of the output currently being captured, set by
    /// the capture stage at (re)init. The input relay maps the client's
    /// normalized touch coordinates onto this rect. `None` until the first
    /// capture stage comes up.
    pub capture_geometry: Arc<Mutex<Option<crate::input::CaptureGeometry>>>,
    /// The client session. Lives here (not in the transport task) so a
    /// pipeline restart — crash recovery, virtual-display toggle, settings
    /// change — does NOT force the iPad through a fresh handshake: the new
    /// transport resumes streaming to the same session and the epoch bump
    /// resets the client's reassembly.
    pub session: Arc<Mutex<crate::transport::session::Session>>,
}

/// Runtime state of the managed virtual extended display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VddStatus {
    /// Not using the managed virtual display (mirroring or a real output).
    Inactive,
    /// Virtual display requested but not yet active because no iPad has connected. The driver
    /// is deliberately left off until a receiver registers, so an idle PC shows no phantom monitor.
    WaitingForClient,
    /// Virtual display enabled and being captured.
    Active,
    /// Virtual display was requested but could not be enabled/attached — capture fell back to
    /// the primary display.
    Failed,
}

/// Selects which display output the capture loop duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    /// Capture the primary desktop output (the one at (0,0)). Default.
    PrimaryAuto,
    /// Capture the output whose DXGI `DeviceName` matches this string (e.g. `\\.\DISPLAY3`).
    Output(String),
    /// Capture the managed virtual extended display. The capture loop enables the bundled
    /// virtual display driver on demand (so no phantom monitor exists when unused), waits
    /// for its output to appear, and disables it again when the target changes.
    VirtualExtended,
}

impl SharedControl {
    pub fn new(listen_port: u16, initial_bitrate_bps: u32) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            bitrate_bps: Arc::new(AtomicU32::new(initial_bitrate_bps)),
            abr_current_bps: Arc::new(AtomicU32::new(initial_bitrate_bps)),
            target_fps: Arc::new(AtomicU32::new(DEFAULT_TARGET_FPS)),
            target_addr: Arc::new(Mutex::new(SocketAddr::from(([0, 0, 0, 0], listen_port)))),
            force_next_idr: Arc::new(AtomicBool::new(false)),
            encoder_override: Arc::new(Mutex::new(None)),
            capture_target: Arc::new(Mutex::new(CaptureTarget::PrimaryAuto)),
            vdd_status: Arc::new(Mutex::new(VddStatus::Inactive)),
            hb_capture_loop_ms: Arc::new(AtomicU64::new(0)),
            hb_capture_frame_ms: Arc::new(AtomicU64::new(0)),
            hb_encode_frame_ms: Arc::new(AtomicU64::new(0)),
            capture_geometry: Arc::new(Mutex::new(None)),
            session: Arc::new(Mutex::new(crate::transport::session::Session::new({
                use std::hash::{BuildHasher, Hasher};
                std::collections::hash_map::RandomState::new()
                    .build_hasher()
                    .finish() as u32
            }))),
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorCommand {
    /// Tear the current pipeline down and start a fresh generation.
    Restart,
    /// Stop streaming (pipeline down, GUI stays). `Start` resumes.
    Stop,
    /// Start a fresh pipeline from Stopped/Failed.
    Start,
    Shutdown,
}

#[derive(Clone)]
pub struct GuiControl {
    pub shared: SharedControl,
    pub supervisor_tx: mpsc::Sender<SupervisorCommand>,
}

impl GuiControl {
    pub fn request_restart(&self) {
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Restart) {
            warn!(error = %error, "Failed to send restart command");
        }
    }

    pub fn request_stop(&self) {
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Stop) {
            warn!(error = %error, "Failed to send stop command");
        }
    }

    pub fn request_start(&self) {
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Start) {
            warn!(error = %error, "Failed to send start command");
        }
    }

    pub fn request_shutdown(&self) {
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Shutdown) {
            warn!(error = %error, "Failed to send shutdown command");
        }
    }
}
