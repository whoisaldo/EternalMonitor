//! Pipeline orchestration: the supervisor loop and per-generation pipeline
//! runner, shared by the binary and the end-to-end tests (which drive a real
//! headless pipeline through the public functions here).

use std::sync::mpsc;
use std::thread::JoinHandle;

use tracing::{error, info};

use crate::control::{SharedControl, SupervisorCommand};
use crate::gpu::GpuInfo;
use crate::{capture, encoder, stats, transport};

pub const DEFAULT_BITRATE_BPS: u32 = 15_000_000;

/// Blocks on the supervisor command channel, owning the pipeline thread across
/// restarts, until `Shutdown` arrives.
pub fn supervisor_loop(
    listen_port: u16,
    shared: SharedControl,
    gpu_info: GpuInfo,
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
                    stats.set_bitrate(shared.bitrate_bps.load(std::sync::atomic::Ordering::SeqCst));
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

pub fn spawn_pipeline_thread(
    listen_port: u16,
    shared: SharedControl,
    gpu_info: GpuInfo,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
) -> JoinHandle<()> {
    shared
        .running
        .store(true, std::sync::atomic::Ordering::SeqCst);
    {
        let mut stats = stats::PIPELINE_STATS.lock();
        stats.mark_pipeline_started();
        stats.set_bitrate(shared.bitrate_bps.load(std::sync::atomic::Ordering::SeqCst));
        stats.set_target_addr(shared.target_addr.lock().to_string());
    }

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async move {
            run_pipeline(listen_port, shared, gpu_info, supervisor_tx).await;
        });
    })
}

pub fn join_pipeline_thread(pipeline_thread: &mut Option<JoinHandle<()>>) {
    if let Some(handle) = pipeline_thread.take() {
        if let Err(error) = handle.join() {
            error!(error = ?error, "Pipeline thread panicked");
        }
    }
}

pub async fn run_pipeline(
    listen_port: u16,
    shared: SharedControl,
    gpu_info: GpuInfo,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
) {
    // ffmpeg_next::init() is idempotent — called here as safety net for restarts
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
        return;
    }

    let capture_rx = capture::start_capture(shared.clone(), gpu_info.adapter_index);
    let nal_rx = encoder::start_encoder(capture_rx, shared.clone(), gpu_info);

    if let Err(e) =
        transport::start_sender(nal_rx, listen_port, shared.clone(), supervisor_tx).await
    {
        error!(error = %e, "Transport sender exited with error");
    }

    stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
    info!("Pipeline ended");
}
