mod capture;
mod encoder;
mod transport;

use std::net::SocketAddr;
use std::time::Instant;

use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let target_addr: SocketAddr = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "192.168.1.255:9876".parse().unwrap());

    info!(%target_addr, "EternalMonitor host starting");

    ffmpeg_next::init()?;

    let pipeline_epoch = Instant::now();
    let capture_rx = capture::start_capture();
    let nal_rx = encoder::start_encoder(capture_rx, 1920, 1080);

    let sender_handle = tokio::spawn(
        transport::start_sender(nal_rx, target_addr, pipeline_epoch),
    );

    if let Err(e) = sender_handle.await? {
        error!(error = %e, "Transport sender exited with error");
    }

    info!("Pipeline ended");
    Ok(())
}
