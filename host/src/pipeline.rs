//! One pipeline generation: capture stage (dedicated thread, latest-wins
//! slot) → encoder stage (dedicated thread, lossless channel) → transport
//! (async). Each stage reports its exit to the supervisor, which owns
//! restarts, backoff, and wedge detection (see `supervisor.rs`).

use std::sync::mpsc;

use tokio::sync::mpsc as tokio_mpsc;
use tracing::{error, info};

use crate::control::{SharedControl, SupervisorCommand};
use crate::gpu::GpuInfo;
use crate::supervisor::{HealthReporter, Stage, StageOutcome};
use crate::{capture, encoder, stats, transport};

pub const DEFAULT_BITRATE_BPS: u32 = 15_000_000;

/// Runs one supervised pipeline generation to completion on the current
/// (tokio) runtime. Stage threads are plain `std::thread`s so a wedged
/// DXGI/encoder call can never block the runtime's shutdown.
pub async fn run_pipeline_supervised(
    generation: u64,
    listen_port: u16,
    shared: SharedControl,
    gpu_info: GpuInfo,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
    reporter: HealthReporter,
) {
    // ffmpeg_next::init() is idempotent — called here as safety net for restarts
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        reporter.stage_exited(
            Stage::Encoder,
            StageOutcome::Failed(format!("ffmpeg init: {e}")),
        );
        stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
        return;
    }

    let (frame_producer, frame_consumer) = capture::frame_slot();
    let (nal_tx, nal_rx) = tokio_mpsc::channel(encoder::NAL_CHANNEL_CAPACITY);

    let capture_reporter = reporter.clone();
    let capture_shared = shared.clone();
    let adapter_index = gpu_info.adapter_index;
    let capture_thread = std::thread::Builder::new()
        .name(format!("capture-g{generation}"))
        .spawn(move || {
            let outcome = match capture::run_capture_stage(
                frame_producer,
                capture_shared,
                adapter_index,
                generation,
            ) {
                Ok(()) => StageOutcome::Completed,
                Err(e) => StageOutcome::Failed(e.to_string()),
            };
            capture_reporter.stage_exited(Stage::Capture, outcome);
        });
    if let Err(e) = capture_thread {
        reporter.stage_exited(Stage::Capture, StageOutcome::Failed(e.to_string()));
        return;
    }

    let encoder_reporter = reporter.clone();
    let encoder_shared = shared.clone();
    let encoder_gpu = gpu_info;
    let encoder_thread = std::thread::Builder::new()
        .name(format!("encode-g{generation}"))
        .spawn(move || {
            let outcome = match encoder::run_encode_stage(
                frame_consumer,
                nal_tx,
                encoder_shared,
                encoder_gpu,
                generation,
            ) {
                Ok(()) => StageOutcome::Completed,
                Err(e) => StageOutcome::Failed(e.to_string()),
            };
            encoder_reporter.stage_exited(Stage::Encoder, outcome);
        });
    if let Err(e) = encoder_thread {
        reporter.stage_exited(Stage::Encoder, StageOutcome::Failed(e.to_string()));
        return;
    }

    let transport_outcome =
        match transport::start_sender(nal_rx, listen_port, shared.clone(), supervisor_tx).await {
            Ok(()) => StageOutcome::Completed,
            Err(e) => {
                error!(error = %e, "Transport sender exited with error");
                StageOutcome::Failed(e.to_string())
            }
        };
    reporter.stage_exited(Stage::Transport, transport_outcome);

    stats::PIPELINE_STATS.lock().mark_pipeline_stopped();
    info!(generation, "Pipeline ended");
}
