pub mod session;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use eternal_wire::v2::media::{MediaHeader, MAX_FRAG_COUNT, MAX_MEDIA_PAYLOAD, MEDIA_HEADER_SIZE};
use eternal_wire::v2::{classify, Classified, MAX_DGRAM_SIZE};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::control::{CaptureTarget, SharedControl, SupervisorCommand, VddStatus};
use crate::encoder::NALUnit;
use crate::stats::PIPELINE_STATS;
use session::{Actions, ConfigSource, Session, HEARTBEAT_INTERVAL};

/// Monotonic per-process counter stamped into each media header's `stream_epoch`. Every
/// `start_sender` (i.e. every pipeline run) gets a fresh value so the iPad can detect a stream
/// restart (seq reset to ~1) reliably instead of inferring it from a sequence-number gap.
static STREAM_EPOCH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Stream parameters advertised in HELLO_ACK and heartbeats.
struct SharedConfigSource<'a> {
    shared: &'a SharedControl,
    stream_epoch: u32,
}

impl ConfigSource for SharedConfigSource<'_> {
    fn stream_config(&self) -> eternal_wire::v2::control::StreamConfig {
        let (width, height, software) = {
            let stats = PIPELINE_STATS.lock();
            let (w, h) = stats.capture_resolution;
            (w, h, stats.using_software_fallback)
        };
        eternal_wire::v2::control::StreamConfig {
            stream_epoch: self.stream_epoch,
            width: width.min(u32::from(u16::MAX)) as u16,
            height: height.min(u32::from(u16::MAX)) as u16,
            fps: self
                .shared
                .target_fps
                .load(Ordering::SeqCst)
                .min(u32::from(u16::MAX)) as u16,
            codec: eternal_wire::v2::control::CODEC_H264,
            flags: if software {
                eternal_wire::v2::control::STREAM_FLAG_SOFTWARE_ENCODER
            } else {
                0
            },
            bitrate_bps: self.shared.bitrate_bps.load(Ordering::SeqCst),
        }
    }

    fn host_name(&self) -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "EternalMonitor".to_string())
    }
}

/// Consumes NAL units from the encoder, fragments each access unit into v2 media
/// datagrams (raw Annex B — no FlatBuffer wrapper), and runs the protocol-v2
/// control plane (hello/ack, heartbeats, keyframe requests, liveness) on the
/// same socket.
pub async fn start_sender(
    mut nal_rx: mpsc::Receiver<NALUnit>,
    listen_port: u16,
    shared: SharedControl,
    supervisor_tx: std_mpsc::Sender<SupervisorCommand>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind_addr: SocketAddr = format!("0.0.0.0:{listen_port}").parse().unwrap();
    let socket = UdpSocket::bind(bind_addr).await?;

    // Never 0 — the receiver treats epoch 0 as invalid. (Wrap after 2^32 runs is
    // harmless: a fresh session id accompanies any host relaunch.)
    let stream_epoch = STREAM_EPOCH_COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
        .max(1);

    let local_addr = socket.local_addr()?;
    info!(%local_addr, stream_epoch, "UDP transport ready — waiting for a v2 client HELLO");
    PIPELINE_STATS
        .lock()
        .set_target_addr(shared.target_addr.lock().to_string());

    // Seed the session-id generator from ambient hasher entropy (no rand dep).
    let seed = {
        use std::hash::{BuildHasher, Hasher};
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish() as u32
    };
    let mut session = Session::new(seed);
    let config = SharedConfigSource {
        shared: &shared,
        stream_epoch,
    };

    let mut recv_buf = [0u8; 2048];
    let mut dgram_scratch = [0u8; MAX_DGRAM_SIZE];
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut liveness_tick = tokio::time::interval(Duration::from_millis(250));
    liveness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            result = socket.recv_from(&mut recv_buf) => {
                match result {
                    Ok((len, src)) => {
                        let datagram = &recv_buf[..len];
                        match classify(datagram) {
                            Classified::Control(_) => {
                                match eternal_wire::v2::control::parse_control(datagram) {
                                    Ok((_, message)) => {
                                        let actions = session.handle_control(
                                            src, message, &config, Instant::now(),
                                        );
                                        execute_actions(
                                            actions, &socket, &shared, &supervisor_tx,
                                        ).await;
                                    }
                                    Err(error) => {
                                        debug!(peer = %src, %error, "Dropped malformed control datagram");
                                    }
                                }
                            }
                            Classified::LegacyHello => session.note_legacy_hello(src),
                            Classified::Media { .. } | Classified::Unknown => {
                                debug!(peer = %src, len, "Ignored unexpected datagram");
                            }
                        }
                    }
                    Err(e) => warn!(error = %e, "recv_from error"),
                }
            }
            nal_opt = nal_rx.recv() => {
                let Some(nal) = nal_opt else { break; };
                let Some(session_id) = session.session_id() else { continue; };
                let target_addr = *shared.target_addr.lock();
                if target_addr.ip().is_unspecified() || target_addr.port() == 0 {
                    continue;
                }

                let send_start = Instant::now();
                let payload = &nal.data;
                let frag_count_usize = payload.len().div_ceil(MAX_MEDIA_PAYLOAD).max(1);
                if frag_count_usize > usize::from(MAX_FRAG_COUNT) {
                    warn!(
                        seq = nal.sequence,
                        total_bytes = payload.len(),
                        "Dropping oversized frame that exceeds the 4 MiB transport cap"
                    );
                    continue;
                }
                let frag_count = frag_count_usize as u16;
                let capture_ts_us = crate::clock::instant_to_us(nal.timestamp);

                let mut send_failed = false;
                for (index, chunk) in payload.chunks(MAX_MEDIA_PAYLOAD).enumerate() {
                    let header = MediaHeader {
                        session_id,
                        stream_epoch,
                        frame_seq: nal.sequence as u32,
                        frag_index: index as u16,
                        frag_count,
                        is_keyframe: nal.is_keyframe,
                        capture_ts_us,
                        payload_len: chunk.len() as u16,
                    };
                    header.encode_into(&mut dgram_scratch);
                    dgram_scratch[MEDIA_HEADER_SIZE..MEDIA_HEADER_SIZE + chunk.len()]
                        .copy_from_slice(chunk);
                    let datagram = &dgram_scratch[..MEDIA_HEADER_SIZE + chunk.len()];

                    if let Err(e) = socket.send_to(datagram, target_addr).await {
                        if !send_failed {
                            warn!(seq = nal.sequence, fragment = index, error = %e, "UDP send failed");
                            send_failed = true;
                        }
                    }
                }

                let total_bytes = payload.len();
                let latency_ms = send_start.elapsed().as_secs_f64() * 1000.0
                    + nal.encode_duration_us as f64 / 1000.0;
                PIPELINE_STATS.lock().record_transport(
                    total_bytes as u64,
                    u64::from(frag_count),
                    latency_ms,
                    target_addr.to_string(),
                );

                debug!(
                    seq = nal.sequence,
                    fragments = frag_count,
                    total_bytes,
                    keyframe = nal.is_keyframe,
                    target = %target_addr,
                    "Frame sent"
                );
            }
            _ = heartbeat.tick() => {
                let actions = session.tick(&config, true, Instant::now());
                execute_actions(actions, &socket, &shared, &supervisor_tx).await;
            }
            _ = liveness_tick.tick() => {
                if !shared.running.load(Ordering::SeqCst) {
                    info!("Transport loop stopping on running=false");
                    break;
                }
                let actions = session.tick(&config, false, Instant::now());
                execute_actions(actions, &socket, &shared, &supervisor_tx).await;
            }
        }
    }

    info!("NAL channel closed, transport sender shutting down");
    Ok(())
}

async fn execute_actions(
    actions: Actions,
    socket: &UdpSocket,
    shared: &SharedControl,
    supervisor_tx: &std_mpsc::Sender<SupervisorCommand>,
) {
    for (destination, datagram) in &actions.replies {
        if let Err(e) = socket.send_to(datagram, destination).await {
            warn!(peer = %destination, error = %e, "Failed to send control reply");
        }
    }

    if let Some(target) = actions.new_target {
        *shared.target_addr.lock() = target;
        {
            let mut stats = PIPELINE_STATS.lock();
            stats.set_target_addr(target.to_string());
            stats.reset_connection_stats();
        }
        // The virtual extended display is enabled lazily by the capture loop's
        // reconcile step, which only acts while a client is connected. If the
        // user selected it before any client existed, this first registration
        // is the moment to bring it up — which needs a pipeline restart so the
        // capture loop re-runs reconciliation.
        let needs_vdd_restart = *shared.capture_target.lock() == CaptureTarget::VirtualExtended
            && *shared.vdd_status.lock() == VddStatus::WaitingForClient;
        if needs_vdd_restart {
            info!("Client connected with extended display selected — restarting pipeline to enable it");
            shared.stop();
            if let Err(error) = supervisor_tx.send(SupervisorCommand::Restart) {
                warn!(error = %error, "Failed to request pipeline restart for the virtual display");
            }
        }
    }

    if actions.force_idr {
        shared.force_next_idr.store(true, Ordering::SeqCst);
    }

    if actions.client_lost {
        let unspecified = SocketAddr::from(([0, 0, 0, 0], 0));
        *shared.target_addr.lock() = unspecified;
        PIPELINE_STATS
            .lock()
            .set_target_addr("waiting for client".to_string());

        // Tear the managed virtual display down when its viewer disappears —
        // the capture reconcile disables it on restart once no client is
        // connected. (This closes the DECISIONS.md "idle-disconnect teardown"
        // item, which was blocked on exactly this liveness signal.)
        let vdd_in_use = *shared.capture_target.lock() == CaptureTarget::VirtualExtended
            && *shared.vdd_status.lock() == VddStatus::Active;
        if vdd_in_use {
            info!("Client gone while streaming the virtual display — restarting pipeline to tear it down");
            shared.stop();
            if let Err(error) = supervisor_tx.send(SupervisorCommand::Restart) {
                warn!(error = %error, "Failed to request pipeline restart after client loss");
            }
        }
    }
}
