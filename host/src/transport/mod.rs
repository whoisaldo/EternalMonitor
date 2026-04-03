pub mod fragment;

use std::net::SocketAddr;
use std::time::Instant;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::encoder::NALUnit;
use crate::stats::PIPELINE_STATS;
use fragment::{FragmentHeader, HEADER_SIZE, MAX_PAYLOAD_SIZE};

/// Magic bytes that the iPad sends to register itself as a receiver.
const HELLO_MAGIC: &[u8] = b"ETERNALHELLO";

/// Consumes NAL units from the encoder, serializes each as a FlatBuffer FramePacket,
/// fragments into MTU-safe UDP datagrams, and sends them to the registered client.
///
/// The sender binds to a known port (from `listen_port`) and waits for a client to
/// send a HELLO_MAGIC registration packet. Once received, frames are streamed to
/// that client's address. If a new client sends HELLO, the target switches.
pub async fn start_sender(
    mut nal_rx: mpsc::Receiver<NALUnit>,
    listen_port: u16,
    pipeline_epoch: Instant,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind_addr: SocketAddr = format!("0.0.0.0:{}", listen_port).parse().unwrap();
    let socket = UdpSocket::bind(bind_addr).await?;
    socket.set_broadcast(true)?;

    let local_addr = socket.local_addr()?;
    info!(%local_addr, "UDP transport ready — waiting for iPad to connect");
    {
        let mut s = PIPELINE_STATS.lock();
        s.target_addr = "waiting for client...".to_string();
    }

    // Wait for the iPad to send a HELLO registration packet.
    let mut client_addr: Option<SocketAddr> = None;
    let mut hello_buf = [0u8; 64];

    // Drain NALs while also checking for HELLO packets.
    // We use a select loop so we can receive both NALs and registration packets.
    loop {
        tokio::select! {
            // Check for incoming registration / keepalive packets
            result = socket.recv_from(&mut hello_buf) => {
                match result {
                    Ok((len, src)) => {
                        if len >= HELLO_MAGIC.len() && &hello_buf[..HELLO_MAGIC.len()] == HELLO_MAGIC {
                            if client_addr.as_ref() != Some(&src) {
                                info!(%src, "iPad registered as receiver");
                                client_addr = Some(src);
                                PIPELINE_STATS.lock().target_addr = src.to_string();
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "recv_from error");
                    }
                }
            }
            // Process NAL units when we have a client
            nal_opt = nal_rx.recv() => {
                let Some(nal) = nal_opt else { break; };

                let Some(target_addr) = client_addr else {
                    // No client registered yet — drop the frame
                    continue;
                };

                let send_start = Instant::now();
                let timestamp_us = nal.timestamp.duration_since(pipeline_epoch).as_micros() as u64;
                let seq = nal.sequence as u32;

                // Detect IDR (keyframe) by scanning for NAL unit type 5 in Annex B stream
                let is_keyframe = detect_idr(&nal.data);

                // Use actual capture resolution from stats
                let (w, h) = PIPELINE_STATS.lock().capture_resolution;
                let width = if w > 0 { w } else { 1920 };
                let height = if h > 0 { h } else { 1080 };

                // Serialize NAL unit as FlatBuffer FramePacket.
                let fb_bytes = eternal_proto::frame::serialize_frame_packet(
                    seq,
                    timestamp_us,
                    &nal.data,
                    width,
                    height,
                    is_keyframe,
                );

                // Fragment and send.
                let total_bytes = fb_bytes.len();
                let chunks: Vec<&[u8]> = fb_bytes.chunks(MAX_PAYLOAD_SIZE).collect();
                let fragment_count = chunks.len().min(255) as u8;

                for (i, chunk) in chunks.iter().enumerate() {
                    let header = FragmentHeader {
                        seq,
                        fragment_index: i as u8,
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

                PIPELINE_STATS
                    .lock()
                    .record_transport(total_bytes as u64, fragment_count as u64);

                let send_us = send_start.elapsed().as_micros();
                info!(
                    seq,
                    fragments = fragment_count,
                    total_bytes,
                    send_us,
                    "Packet sent"
                );
            }
        }
    }

    info!("NAL channel closed, transport sender shutting down");
    Ok(())
}

/// Scan an Annex B NAL bitstream for IDR slice (NAL type 5) indicating a keyframe.
fn detect_idr(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < data.len() {
        // Look for start code (0x00 0x00 0x01) or (0x00 0x00 0x00 0x01)
        if data[i] == 0 && data[i + 1] == 0 {
            let (nal_start, _) = if data[i + 2] == 1 {
                (i + 3, 3)
            } else if i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                (i + 4, 4)
            } else {
                i += 1;
                continue;
            };
            if nal_start < data.len() {
                let nal_type = data[nal_start] & 0x1F;
                if nal_type == 5 {
                    return true;
                }
            }
            i = nal_start;
        } else {
            i += 1;
        }
    }
    false
}
