pub mod fragment;

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::control::SharedControl;
use crate::encoder::NALUnit;
use crate::stats::PIPELINE_STATS;
use fragment::{FragmentHeader, HEADER_SIZE, MAX_PAYLOAD_SIZE};

/// Magic bytes that the iPad sends to register itself as a receiver.
const HELLO_MAGIC: &[u8] = b"ETERNALHELLO";

/// Consumes NAL units from the encoder, serializes each as a FlatBuffer FramePacket,
/// fragments into MTU-safe UDP datagrams, and sends them to the current target address.
pub async fn start_sender(
    mut nal_rx: mpsc::Receiver<NALUnit>,
    listen_port: u16,
    pipeline_epoch: Instant,
    shared: SharedControl,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind_addr: SocketAddr = format!("0.0.0.0:{listen_port}").parse().unwrap();
    let socket = UdpSocket::bind(bind_addr).await?;
    socket.set_broadcast(true)?;

    let local_addr = socket.local_addr()?;
    info!(%local_addr, "UDP transport ready — waiting for iPad to connect");
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
                            *shared.target_addr.lock() = target;
                            PIPELINE_STATS.lock().set_target_addr(target.to_string());
                            info!(%target, "iPad registered as receiver");
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
                info!(
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
