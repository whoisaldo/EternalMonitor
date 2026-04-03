mod capture;
mod discovery;
mod encoder;
mod gui;
mod stats;
mod transport;

use std::net::SocketAddr;
use std::time::Instant;

use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    // Mark pipeline as running
    {
        let mut s = stats::PIPELINE_STATS.lock();
        s.pipeline_running = true;
        s.start_time = Some(Instant::now());
    }

    // Spawn the pipeline on a background tokio runtime thread
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async move {
            run_pipeline(target_addr).await;
        });
    });

    // Advertise via mDNS so iOS scanner can find us (keep handle alive)
    let _mdns = discovery::advertise_service(target_addr);

    // Run the GUI on the main thread (blocks until window is closed)
    if let Err(e) = gui::run_gui() {
        error!(error = %e, "GUI exited with error");
    }

    info!("EternalMonitor shutting down");
    Ok(())
}

async fn run_pipeline(target_addr: SocketAddr) {
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        stats::PIPELINE_STATS.lock().pipeline_running = false;
        return;
    }

    let pipeline_epoch = Instant::now();
    let capture_rx = capture::start_capture();
    let nal_rx = encoder::start_encoder(capture_rx, 1920, 1080);

    if let Err(e) = transport::start_sender(nal_rx, target_addr, pipeline_epoch).await {
        error!(error = %e, "Transport sender exited with error");
    }

    stats::PIPELINE_STATS.lock().pipeline_running = false;
    info!("Pipeline ended");
}
