//! Lifecycle control for the bundled third-party virtual display driver (VDD).
//!
//! The driver is left DISABLED by default so no phantom monitor exists when EternalMonitor
//! isn't using it. We enable it only while actively capturing the virtual extended display.
//!
//! Enabling/disabling a driver device requires admin, so the installer registers two
//! "run with highest privileges" scheduled tasks (one per direction). Triggering an
//! already-elevated, pre-authorized task does not raise a UAC prompt, so the non-elevated
//! host can flip the virtual display on and off seamlessly.

use std::process::Command;

use tracing::{info, warn};

/// Scheduled task names — must match the ones the installer registers.
pub const TASK_ENABLE: &str = "EternalMonitor VDD Enable";
pub const TASK_DISABLE: &str = "EternalMonitor VDD Disable";

/// Trigger a pre-registered scheduled task by name. Returns true on success. Missing tasks
/// (e.g. a build installed without the VDD feature) just log a warning and return false —
/// the caller falls back to the primary display.
fn run_task(task: &str) -> bool {
    let mut command = Command::new("schtasks");
    command.args(["/Run", "/TN", task]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    match command.output() {
        Ok(out) if out.status.success() => {
            info!(task, "Triggered VDD scheduled task");
            true
        }
        Ok(out) => {
            warn!(
                task,
                code = ?out.status.code(),
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "VDD scheduled task did not run — is the installer's task registered?"
            );
            false
        }
        Err(error) => {
            warn!(task, error = %error, "Failed to invoke schtasks for VDD control");
            false
        }
    }
}

/// Enable the virtual display device. Returns false if the task could not be triggered.
pub fn enable() -> bool {
    run_task(TASK_ENABLE)
}

/// Disable the virtual display device (best effort — failures are non-fatal).
pub fn disable() {
    run_task(TASK_DISABLE);
}
