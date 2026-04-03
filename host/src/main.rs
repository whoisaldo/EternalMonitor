mod capture;

use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    info!("EternalMonitor host starting");

    // Run the blocking DXGI capture loop on a dedicated thread.
    let capture_handle = tokio::task::spawn_blocking(|| {
        if let Err(e) = capture::run_capture_loop() {
            tracing::error!(error = %e, "Capture loop exited with error");
        }
    });

    capture_handle.await?;

    Ok(())
}
