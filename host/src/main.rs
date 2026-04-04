mod capture;
mod control;
mod discovery;
mod encoder;
mod gui;
mod logging;
mod stats;
mod transport;

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use control::{GuiControl, SharedControl, SupervisorCommand};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::prelude::*;

const DEFAULT_BITRATE_BPS: u32 = 15_000_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stdout.with_max_level(tracing::Level::INFO));
    let memory_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(|| logging::MemoryLogWriter::new());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(memory_layer)
        .init();

    let listen_port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(9876);

    info!(listen_port, "EternalMonitor host starting");

    let shared = SharedControl::new(listen_port, DEFAULT_BITRATE_BPS);
    {
        let mut stats = stats::PIPELINE_STATS.lock();
        stats.set_bitrate(DEFAULT_BITRATE_BPS);
        stats.set_listen_addr(gui::detect_local_ip(listen_port));
        stats.set_target_addr(shared.target_addr.lock().to_string());
    }

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let gui_control = GuiControl {
        shared: shared.clone(),
        supervisor_tx: supervisor_tx.clone(),
    };

    let supervisor_thread = std::thread::spawn(move || {
        supervisor_loop(listen_port, shared, supervisor_rx);
    });

    let _mdns = discovery::advertise_service(listen_port);

    if let Err(e) = gui::run_gui(gui_control.clone()) {
        error!(error = %e, "GUI exited with error");
    }

    gui_control.shared.stop();
    gui_control.request_shutdown();

    if let Err(error) = supervisor_thread.join() {
        error!(error = ?error, "Supervisor thread panicked");
    }

    info!("EternalMonitor shutting down");
    Ok(())
}

fn supervisor_loop(
    listen_port: u16,
    shared: SharedControl,
    supervisor_rx: mpsc::Receiver<SupervisorCommand>,
) {
    let mut pipeline_thread = Some(spawn_pipeline_thread(listen_port, shared.clone()));

    while let Ok(command) = supervisor_rx.recv() {
        match command {
            SupervisorCommand::Restart => {
                info!("Restarting pipeline");
                shared.stop();
                join_pipeline_thread(&mut pipeline_thread);

                {
                    let mut stats = stats::PIPELINE_STATS.lock();
                    stats.reset_for_restart();
                    stats.set_target_addr(shared.target_addr.lock().to_string());
                    stats.set_bitrate(
                        shared
                            .bitrate_bps
                            .load(std::sync::atomic::Ordering::SeqCst),
                    );
                }

                pipeline_thread = Some(spawn_pipeline_thread(listen_port, shared.clone()));
            }
            SupervisorCommand::Shutdown => {
                info!("Shutting down pipeline supervisor");
                shared.stop();
                join_pipeline_thread(&mut pipeline_thread);
                break;
            }
        }
    }
}

fn spawn_pipeline_thread(listen_port: u16, shared: SharedControl) -> JoinHandle<()> {
    shared
        .running
        .store(true, std::sync::atomic::Ordering::SeqCst);
    {
        let mut stats = stats::PIPELINE_STATS.lock();
        stats.mark_pipeline_started();
        stats.set_bitrate(
            shared
                .bitrate_bps
                .load(std::sync::atomic::Ordering::SeqCst),
        );
        stats.set_target_addr(shared.target_addr.lock().to_string());
    }

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async move {
            run_pipeline(listen_port, shared).await;
        });
    })
}

fn join_pipeline_thread(pipeline_thread: &mut Option<JoinHandle<()>>) {
    if let Some(handle) = pipeline_thread.take() {
        if let Err(error) = handle.join() {
            error!(error = ?error, "Pipeline thread panicked");
        }
    }
}

async fn run_pipeline(listen_port: u16, shared: SharedControl) {
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
        return;
    }

    let pipeline_epoch = Instant::now();
    let capture_rx = capture::start_capture(shared.clone());
    let nal_rx = encoder::start_encoder(capture_rx, shared.clone());

    if let Err(e) = transport::start_sender(nal_rx, listen_port, pipeline_epoch, shared.clone()).await
    {
        error!(error = %e, "Transport sender exited with error");
    }

    stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
    info!("Pipeline ended");
}
