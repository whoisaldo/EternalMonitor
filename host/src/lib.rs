//! EternalMonitor host library. The binary (`main.rs`) is a thin bootstrap
//! over these modules; end-to-end tests drive the same pipeline headlessly.

pub mod autostart;
pub mod capture;
pub mod control;
pub mod discovery;
pub mod encoder;
pub mod gui;
pub mod logging;
pub mod pipeline;
pub mod settings;
pub mod stats;
pub mod transport;
pub mod vdd;

#[cfg(windows)]
pub mod gpu;
#[cfg(not(windows))]
#[path = "gpu_other.rs"]
pub mod gpu;
