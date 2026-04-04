use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use parking_lot::Mutex;
use tracing::warn;

#[derive(Clone)]
pub struct SharedControl {
    pub running: Arc<AtomicBool>,
    pub bitrate_bps: Arc<AtomicU32>,
    pub target_addr: Arc<Mutex<SocketAddr>>,
    pub last_receiver_restart_at: Arc<Mutex<Option<Instant>>>,
}

impl SharedControl {
    pub fn new(listen_port: u16, initial_bitrate_bps: u32) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            bitrate_bps: Arc::new(AtomicU32::new(initial_bitrate_bps)),
            target_addr: Arc::new(Mutex::new(SocketAddr::from(([0, 0, 0, 0], listen_port)))),
            last_receiver_restart_at: Arc::new(Mutex::new(None)),
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
