pub mod fragment;

use std::net::SocketAddr;
use std::sync::mpsc as std_mpsc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::control::{SharedControl, SupervisorCommand};
use crate::encoder::NALUnit;
use crate::stats::PIPELINE_STATS;
use fragment::{FragmentHeader, HEADER_SIZE, MAX_PAYLOAD_SIZE};

/// Magic bytes that the iPad sends to register itself as a receiver.
const HELLO_MAGIC: &[u8] = b"ETERNALHELLO";

/// Monotonic per-process counter stamped into each fragment header's `stream_epoch`. Every
/// `start_sender` (i.e. every pipeline run) gets a fresh value so the iPad can detect a stream
/// restart (seq reset to ~1) reliably instead of inferring it from a sequence-number gap.
static STREAM_EPOCH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Consumes NAL units from the encoder, serializes each as a FlatBuffer FramePacket,
/// fragments into MTU-safe UDP datagrams, and sends them to the current target address.
pub async fn start_sender(
    mut nal_rx: mpsc::Receiver<NALUnit>,
    listen_port: u16,
    pipeline_epoch: Instant,
    shared: SharedControl,
    supervisor_tx: std_mpsc::Sender<SupervisorCommand>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind_addr: SocketAddr = format!("0.0.0.0:{listen_port}").parse().unwrap();
    let socket = UdpSocket::bind(bind_addr).await?;
    socket.set_broadcast(true)?;

    // Monotonically increasing per pipeline run. Never 0 — the receiver treats 0 as "legacy host
    // without epoch support" and falls back to seq-gap restart detection. (Wrap after 2^32 runs is
    // harmless: the receiver also keeps the seq-gap fallback.)
    let stream_epoch = STREAM_EPOCH_COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
        .max(1);

    let local_addr = socket.local_addr()?;
    info!(%local_addr, stream_epoch, "UDP transport ready — waiting for iPad to connect");
    PIPELINE_STATS
        .lock()
        .set_target_addr(shared.target_addr.lock().to_string());

    let mut hello_buf = [0u8; 64];

    loop {
        tokio::select! {
            result = socket.recv_from(&mut hello_buf) => {
                match result {
                    Ok((len, src)) => {
                        if len >= HELLO_MAGIC.len() && &hello_buf[..HELLO_MAGIC.len()] == HELLO_MAGIC {
                            let target = if len >= HELLO_MAGIC.len() + 2 {
                                let offset = HELLO_MAGIC.len();
                                let listen_port = u16::from_le_bytes([hello_buf[offset], hello_buf[offset + 1]]);
                                SocketAddr::new(src.ip(), listen_port)
                            } else {
                                src
                            };
                            let previous_target = *shared.target_addr.lock();
                            *shared.target_addr.lock() = target;
                            PIPELINE_STATS.lock().set_target_addr(target.to_string());
                            info!(%target, "iPad registered as receiver");

                            match receiver_restart_reason(previous_target, target) {
                                Some(reason) => {
                                    info!(
                                        reason,
                                        previous = %previous_target,
                                        current = %target,
                                        "Receiver registration changed; restarting pipeline to send a fresh startup keyframe"
                                    );
                                    shared.stop();
                                    if let Err(error) = supervisor_tx.send(SupervisorCommand::Restart) {
                                        warn!(error = %error, "Failed to request pipeline restart after receiver registration");
                                    }
                                    info!("Transport loop exiting immediately after restart request");
                                    break;
                                }
                                None => {
                                    // Same-target re-handshake. Don't restart the pipeline — just
                                    // force the next encoded frame to be an IDR and clear connection
                                    // counters so the GUI shows fresh session stats.
                                    shared.force_next_idr.store(true, Ordering::SeqCst);
                                    PIPELINE_STATS.lock().reset_connection_stats();
                                    info!(
                                        %target,
                                        "New client connected — forcing immediate IDR on next frame"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => warn!(error = %e, "recv_from error"),
                }
            }
            nal_opt = nal_rx.recv() => {
                let Some(nal) = nal_opt else { break; };
                let target_addr = *shared.target_addr.lock();
                if target_addr.ip().is_unspecified() || target_addr.port() == 0 {
                    continue;
                }

                let send_start = Instant::now();
                let timestamp_us = nal.timestamp.duration_since(pipeline_epoch).as_micros() as u64;
                let seq = nal.sequence as u32;
                let is_keyframe = nal.is_keyframe;

                let (width, height) = {
                    let stats = PIPELINE_STATS.lock();
                    let (w, h) = stats.capture_resolution;
                    (w.max(1), h.max(1))
                };

                let fb_bytes = eternal_proto::frame::serialize_frame_packet(
                    seq,
                    timestamp_us,
                    &nal.data,
                    width,
                    height,
                    is_keyframe,
                );

                let total_bytes = fb_bytes.len();
                let chunks: Vec<&[u8]> = fb_bytes.chunks(MAX_PAYLOAD_SIZE).collect();
                if chunks.len() > u16::MAX as usize {
                    warn!(
                        seq,
                        total_bytes,
                        fragments = chunks.len(),
                        "Dropping oversized frame that exceeds transport fragment limit"
                    );
                    continue;
                }
                let fragment_count = chunks.len() as u16;

                for (i, chunk) in chunks.iter().enumerate() {
                    let header = FragmentHeader {
                        seq,
                        fragment_index: i as u16,
                        fragment_count,
                        payload_len: chunk.len() as u32,
                        stream_epoch,
                    };

                    let mut dgram = Vec::with_capacity(HEADER_SIZE + chunk.len());
                    dgram.extend_from_slice(&header.to_bytes());
                    dgram.extend_from_slice(chunk);

                    if let Err(e) = socket.send_to(&dgram, target_addr).await {
                        warn!(seq, fragment = i, error = %e, "UDP send failed");
                    }
                }

                let latency_ms = send_start.elapsed().as_secs_f64() * 1000.0
                    + nal.encode_duration_us as f64 / 1000.0;
                PIPELINE_STATS.lock().record_transport(
                    total_bytes as u64,
                    fragment_count as u64,
                    latency_ms,
                    target_addr.to_string(),
                );

                let send_us = send_start.elapsed().as_micros();
                debug!(
                    seq,
                    fragments = fragment_count,
                    total_bytes,
                    send_us,
                    target = %target_addr,
                    "Packet sent"
                );
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if !shared.running.load(Ordering::SeqCst) {
                    info!("Transport loop stopping on running=false");
                    break;
                }
            }
        }
    }

    info!("NAL channel closed, transport sender shutting down");
    Ok(())
}

/// Decides whether an iPad (re-)registration warrants a full pipeline restart.
/// Returns `Some(reason)` only for a first registration or a changed target; a same-target
/// re-handshake returns `None`, signalling the caller to just force an IDR without restarting.
fn receiver_restart_reason(
    previous_target: SocketAddr,
    current_target: SocketAddr,
) -> Option<&'static str> {
    if previous_target.ip().is_unspecified() || previous_target.port() == 0 {
        return Some("first receiver registration");
    }

    if previous_target != current_target {
        return Some("receiver target changed");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::receiver_restart_reason;
    use std::net::SocketAddr;

    #[test]
    fn restarts_for_first_receiver_registration() {
        let previous = SocketAddr::from(([0, 0, 0, 0], 9876));
        let current = SocketAddr::from(([10, 0, 0, 50], 9876));
        assert_eq!(
            receiver_restart_reason(previous, current),
            Some("first receiver registration")
        );
    }

    #[test]
    fn restarts_when_receiver_target_changes() {
        let previous = SocketAddr::from(([10, 0, 0, 50], 9876));
        let current = SocketAddr::from(([10, 0, 0, 51], 9876));
        assert_eq!(
            receiver_restart_reason(previous, current),
            Some("receiver target changed")
        );
    }

    #[test]
    fn same_target_rehandshake_does_not_restart() {
        let current = SocketAddr::from(([10, 0, 0, 50], 9876));
        assert_eq!(receiver_restart_reason(current, current), None);
    }
}
