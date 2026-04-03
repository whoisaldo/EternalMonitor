pub mod fragment;

use std::net::SocketAddr;
use std::time::Instant;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::encoder::NALUnit;
use fragment::{FragmentHeader, HEADER_SIZE, MAX_PAYLOAD_SIZE};

/// Consumes NAL units from the encoder, serializes each as a FlatBuffer FramePacket,
/// fragments into MTU-safe UDP datagrams, and sends them to `target_addr`.
///
/// Runs as an async task via `tokio::spawn`.
pub async fn start_sender(
    mut nal_rx: mpsc::Receiver<NALUnit>,
    target_addr: SocketAddr,
    pipeline_epoch: Instant,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;

    let local_addr = socket.local_addr()?;
    info!(%local_addr, %target_addr, "UDP transport sender started");

    while let Some(nal) = nal_rx.recv().await {
        let send_start = Instant::now();

        let timestamp_us = nal
            .timestamp
            .duration_since(pipeline_epoch)
            .as_micros() as u64;

        let seq = nal.sequence as u32;

        // Serialize NAL unit as FlatBuffer FramePacket.
        let fb_bytes = eternal_proto::frame::serialize_frame_packet(
            seq,
            timestamp_us,
            &nal.data,
            1920,
            1080,
            false, // TODO: detect keyframes from NAL unit type
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

        let send_us = send_start.elapsed().as_micros();
        info!(
            seq,
            fragments = fragment_count,
            total_bytes,
            send_us,
            "Packet sent"
        );
    }

    info!("NAL channel closed, transport sender shutting down");
    Ok(())
}
