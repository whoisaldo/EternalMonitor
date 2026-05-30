use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};

use parking_lot::Mutex;
use tracing::warn;

pub const DEFAULT_TARGET_FPS: u32 = 60;

#[derive(Clone)]
pub struct SharedControl {
    pub running: Arc<AtomicBool>,
    pub bitrate_bps: Arc<AtomicU32>,
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
            target_fps: Arc::new(AtomicU32::new(DEFAULT_TARGET_FPS)),
            target_addr: Arc::new(Mutex::new(SocketAddr::from(([0, 0, 0, 0], listen_port)))),
            force_next_idr: Arc::new(AtomicBool::new(false)),
            encoder_override: Arc::new(Mutex::new(None)),
            capture_target: Arc::new(Mutex::new(CaptureTarget::PrimaryAuto)),
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SupervisorCommand {
    Restart,
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

    pub fn request_shutdown(&self) {
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Shutdown) {
            warn!(error = %error, "Failed to send shutdown command");
        }
    }
}
