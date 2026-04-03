use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::capture::RawFrame;
use crate::stats::PIPELINE_STATS;

const CHANNEL_CAPACITY: usize = 4;
const DEFAULT_BITRATE: usize = 15_000_000; // 15 Mbps

/// Encoded H.264 NAL unit ready for transport.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct NALUnit {
    pub data: Vec<u8>,
    pub sequence: u64,
    pub timestamp: Instant,
    pub encode_duration_us: u128,
}

/// Starts the NVENC H.264 encode loop on a dedicated blocking thread.
/// Consumes `RawFrame`s from the capture stage and produces `NALUnit`s.
pub fn start_encoder(
    rx: mpsc::Receiver<RawFrame>,
    width: u32,
    height: u32,
) -> mpsc::Receiver<NALUnit> {
    let (tx, nal_rx) = mpsc::channel::<NALUnit>(CHANNEL_CAPACITY);

    tokio::task::spawn_blocking(move || {
        if let Err(e) = run_encode_loop(rx, tx, width, height) {
            error!(error = %e, "Encode loop exited with error");
        }
    });

    nal_rx
}

fn run_encode_loop(
    rx: mpsc::Receiver<RawFrame>,
    tx: mpsc::Sender<NALUnit>,
    width: u32,
    height: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // --- Find NVENC codec ---
    let codec = ffmpeg_next::encoder::find_by_name("h264_nvenc")
        .ok_or("h264_nvenc codec not found — is NVENC-enabled FFmpeg installed?")?;
    info!("Found h264_nvenc codec");

    // --- Configure encoder ---
    let context = ffmpeg_next::codec::Context::new_with_codec(codec);
    let mut encoder = context.encoder().video()?;

    encoder.set_width(width);
    encoder.set_height(height);
    encoder.set_format(ffmpeg_next::format::Pixel::YUV420P);
    encoder.set_time_base(ffmpeg_next::Rational(1, 60));
    encoder.set_max_b_frames(0);
    encoder.set_bit_rate(DEFAULT_BITRATE);

    let mut opts = ffmpeg_next::Dictionary::new();
    opts.set("preset", "p1");
    opts.set("tune", "ll");
    opts.set("profile", "baseline");
    opts.set("zerolatency", "1");
    opts.set("rc", "cbr");

    let mut encoder = encoder.open_with(opts)?;
    info!(
        width,
        height,
        bitrate = DEFAULT_BITRATE,
        "NVENC encoder opened"
    );

    // --- swscale BGRA → YUV420P ---
    let mut scaler = ffmpeg_next::software::scaling::Context::get(
        ffmpeg_next::format::Pixel::BGRA,
        width,
        height,
        ffmpeg_next::format::Pixel::YUV420P,
        width,
        height,
        ffmpeg_next::software::scaling::Flags::BILINEAR,
    )?;
    info!("swscale BGRA->YUV420P context created");

    // --- Reusable frames and packet ---
    let mut bgra_frame =
        ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::BGRA, width, height);
    let mut frame =
        ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, width, height);
    let mut packet = ffmpeg_next::Packet::empty();

    // --- Blocking receive loop ---
    let mut rx = rx;
    while let Some(raw_frame) = rx.blocking_recv() {
        if raw_frame.data.is_empty() {
            warn!(
                frame = raw_frame.frame_number,
                "Skipping frame with empty data"
            );
            continue;
        }

        // Fill BGRA input frame row-by-row (stride may differ from width*4)
        let stride = bgra_frame.stride(0);
        let src_row_bytes = (width * 4) as usize;
        let bgra_plane = bgra_frame.data_mut(0);
        for y in 0..height as usize {
            let src_offset = y * src_row_bytes;
            let dst_offset = y * stride;
            bgra_plane[dst_offset..dst_offset + src_row_bytes]
                .copy_from_slice(&raw_frame.data[src_offset..src_offset + src_row_bytes]);
        }

        // Convert BGRA → YUV420P
        scaler.run(&bgra_frame, &mut frame)?;
        frame.set_pts(Some(raw_frame.frame_number as i64));

        let encode_start = Instant::now();
        encoder.send_frame(&frame)?;

        // Drain all packets produced for this frame
        loop {
            match encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let encode_us = encode_start.elapsed().as_micros();
                    let nal_data = packet.data().map(|d| d.to_vec()).unwrap_or_default();

                    PIPELINE_STATS
                        .lock()
                        .record_encode(encode_us, nal_data.len());

                    info!(
                        frame = raw_frame.frame_number,
                        encode_us = encode_us,
                        nal_bytes = nal_data.len(),
                        real_frame = true,
                        "Frame encoded"
                    );

                    let nal = NALUnit {
                        data: nal_data,
                        sequence: raw_frame.frame_number,
                        timestamp: raw_frame.timestamp,
                        encode_duration_us: encode_us,
                    };

                    if tx.blocking_send(nal).is_err() {
                        info!("NAL channel closed, stopping encoder");
                        return Ok(());
                    }
                }
                Err(e) if is_eagain(&e) => break,
                Err(e) => return Err(e.into()),
            }
        }
    }

    info!("Capture channel closed, encoder shutting down");
    Ok(())
}

/// Check if an ffmpeg error is EAGAIN (no output available yet).
fn is_eagain(e: &ffmpeg_next::Error) -> bool {
    matches!(e, ffmpeg_next::Error::Other { errno } if *errno == libc::EAGAIN)
}
