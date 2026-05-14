mod capture;
mod control;
mod discovery;
mod encoder;
mod gpu;
mod gui;
mod logging;
mod settings;
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
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=warn"));
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stdout.with_max_level(tracing::Level::INFO))
        .with_filter(logging::MdnsDedupFilter::new());
    let memory_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(|| logging::MemoryLogWriter::new())
        .with_filter(logging::MdnsDedupFilter::new());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(memory_layer)
        .init();

    info!(
        path = %logging::session_log_path().display(),
        "Session log file initialized"
    );

    let listen_port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(9876);

    // Initialize FFmpeg early so encoder probing works during GPU detection
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        return Err(e.into());
    }

    let gpu_info = gpu::GpuInfo::detect();

    info!("══════════════════════════════════");
    info!("  EternalMonitor v0.1.1");
    info!("  GPU:     {} ({})", gpu_info.name, gpu_info.vendor);
    info!("  VRAM:    {} MB", gpu_info.dedicated_vram_mb);
    info!("  Encoder: {}", gpu_info.codec_display_name);
    info!("  Listen:  0.0.0.0:{}", listen_port);
    info!("══════════════════════════════════");

    let shared = SharedControl::new(listen_port, DEFAULT_BITRATE_BPS);
    {
        let mut stats = stats::PIPELINE_STATS.lock();
        stats.set_bitrate(DEFAULT_BITRATE_BPS);
        stats.set_listen_addr(gui::detect_local_ip(listen_port));
        stats.set_target_addr(shared.target_addr.lock().to_string());
        stats.set_gpu_name(gpu_info.name.clone());
        stats.set_codec_name(gpu_info.codec_display_name.clone());
    }

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let gui_control = GuiControl {
        shared: shared.clone(),
        supervisor_tx: supervisor_tx.clone(),
    };

    let supervisor_thread = std::thread::spawn(move || {
        supervisor_loop(listen_port, shared, gpu_info, supervisor_tx, supervisor_rx);
    });

    let _mdns = discovery::advertise_service(listen_port);

    if let Err(e) = gui::run_gui(gui_control.clone()) {
        error!(error = %e, "GUI exited with error");
    }

    gui_control.shared.stop();
    gui_control.request_shutdown();

    // Give the supervisor a bounded amount of time to shut down cleanly.
    // If it doesn't finish (blocked on DXGI acquire, mDNS threads, etc.),
    // force-exit so the process never lingers as an invisible zombie.
    let shutdown_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    std::thread::spawn(move || {
        if let Err(error) = supervisor_thread.join() {
            error!(error = ?error, "Supervisor thread panicked");
        }
    });

    while std::time::Instant::now() < shutdown_deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    info!("EternalMonitor shutting down");
    std::process::exit(0);
}

fn supervisor_loop(
    listen_port: u16,
    shared: SharedControl,
    gpu_info: gpu::GpuInfo,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
    supervisor_rx: mpsc::Receiver<SupervisorCommand>,
) {
    let mut pipeline_thread = Some(spawn_pipeline_thread(
        listen_port,
        shared.clone(),
        gpu_info.clone(),
        supervisor_tx.clone(),
    ));

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

                pipeline_thread = Some(spawn_pipeline_thread(
                    listen_port,
                    shared.clone(),
                    gpu_info.clone(),
                    supervisor_tx.clone(),
                ));
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

fn spawn_pipeline_thread(
    listen_port: u16,
    shared: SharedControl,
    gpu_info: gpu::GpuInfo,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
) -> JoinHandle<()> {
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
            run_pipeline(listen_port, shared, gpu_info, supervisor_tx).await;
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

async fn run_pipeline(
    listen_port: u16,
    shared: SharedControl,
    gpu_info: gpu::GpuInfo,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
) {
    // ffmpeg_next::init() is idempotent — called here as safety net for restarts
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
        return;
    }

    let pipeline_epoch = Instant::now();
    let capture_rx = capture::start_capture(shared.clone(), gpu_info.adapter_index);
    let nal_rx = encoder::start_encoder(capture_rx, shared.clone(), gpu_info);

    if let Err(e) = transport::start_sender(
        nal_rx,
        listen_port,
        pipeline_epoch,
        shared.clone(),
        supervisor_tx,
    )
    .await
    {
        error!(error = %e, "Transport sender exited with error");
    }

    stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
    info!("Pipeline ended");
}
