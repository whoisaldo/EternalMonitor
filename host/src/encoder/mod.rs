use std::sync::atomic::Ordering;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::capture::RawFrame;
use crate::control::SharedControl;
use crate::stats::PIPELINE_STATS;

const CHANNEL_CAPACITY: usize = 4;
const DEFAULT_BITRATE: u32 = 15_000_000;
const CODEC_NAME: &str = "H.264 (NVENC)";

/// Encoded H.264 NAL unit ready for transport.
#[derive(Debug, Clone)]
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
    shared: SharedControl,
) -> mpsc::Receiver<NALUnit> {
    let (tx, nal_rx) = mpsc::channel::<NALUnit>(CHANNEL_CAPACITY);

    tokio::task::spawn_blocking(move || {
        if let Err(e) = run_encode_loop(rx, tx, shared) {
            error!(error = %e, "Encode loop exited with error");
        }
    });

    nal_rx
}

fn run_encode_loop(
    rx: mpsc::Receiver<RawFrame>,
    tx: mpsc::Sender<NALUnit>,
    shared: SharedControl,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let codec = ffmpeg_next::encoder::find_by_name("h264_nvenc")
        .ok_or("h264_nvenc codec not found — is NVENC-enabled FFmpeg installed?")?;
    info!("Found h264_nvenc codec");
    PIPELINE_STATS.lock().set_codec_name(CODEC_NAME);

    let mut encoder_state: Option<EncoderState> = None;
    let mut rx = rx;

    while let Some(raw_frame) = rx.blocking_recv() {
        if !shared.running.load(Ordering::SeqCst) {
            info!("Encoder loop stopping on running=false");
            break;
        }

        if raw_frame.data.is_empty() {
            warn!(
                frame = raw_frame.frame_number,
                "Skipping frame with empty data"
            );
            continue;
        }

        if encoder_state.is_none() {
            encoder_state = Some(EncoderState::new(codec, raw_frame.width, raw_frame.height)?);
        }

        let encoder = encoder_state
            .as_mut()
            .expect("Encoder state must be initialized");

        let current_bitrate = shared.bitrate_bps.load(Ordering::SeqCst);
        apply_bitrate(&mut encoder.encoder, current_bitrate)?;

        let stride = encoder.bgra_frame.stride(0);
        let src_row_bytes = (raw_frame.width * 4) as usize;
        let bgra_plane = encoder.bgra_frame.data_mut(0);
        for y in 0..raw_frame.height as usize {
            let src_offset = y * src_row_bytes;
            let dst_offset = y * stride;
            bgra_plane[dst_offset..dst_offset + src_row_bytes]
                .copy_from_slice(&raw_frame.data[src_offset..src_offset + src_row_bytes]);
        }

        encoder
            .scaler
            .run(&encoder.bgra_frame, &mut encoder.frame)?;
        encoder.frame.set_pts(Some(raw_frame.frame_number as i64));

        let encode_start = Instant::now();
        encoder.encoder.send_frame(&encoder.frame)?;

        loop {
            match encoder.encoder.receive_packet(&mut encoder.packet) {
                Ok(()) => {
                    let encode_us = encode_start.elapsed().as_micros();
                    let nal_data = encoder
                        .packet
                        .data()
                        .map(|data| data.to_vec())
                        .unwrap_or_default();

                    PIPELINE_STATS
                        .lock()
                        .record_encode(encode_us, nal_data.len(), current_bitrate);

                    info!(
                        frame = raw_frame.frame_number,
                        encode_us,
                        nal_bytes = nal_data.len(),
                        bitrate_bps = current_bitrate,
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

struct EncoderState {
    encoder: ffmpeg_next::codec::encoder::video::Encoder,
    scaler: ffmpeg_next::software::scaling::Context,
    bgra_frame: ffmpeg_next::frame::Video,
    frame: ffmpeg_next::frame::Video,
    packet: ffmpeg_next::Packet,
}

impl EncoderState {
    fn new(
        codec: ffmpeg_next::Codec,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let context = ffmpeg_next::codec::Context::new_with_codec(codec);
        let mut encoder = context.encoder().video()?;

        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg_next::format::Pixel::YUV420P);
        encoder.set_time_base(ffmpeg_next::Rational(1, 60));
        encoder.set_max_b_frames(0);
        encoder.set_bit_rate(DEFAULT_BITRATE as usize);
        encoder.set_gop(30);

        let mut opts = ffmpeg_next::Dictionary::new();
        opts.set("preset", "p1");
        opts.set("tune", "ll");
        opts.set("profile", "baseline");
        opts.set("zerolatency", "1");
        opts.set("rc", "cbr");

        let encoder = encoder.open_with(opts)?;
        info!(width, height, bitrate = DEFAULT_BITRATE, "NVENC encoder opened");

        let scaler = ffmpeg_next::software::scaling::Context::get(
            ffmpeg_next::format::Pixel::BGRA,
            width,
            height,
            ffmpeg_next::format::Pixel::YUV420P,
            width,
            height,
            ffmpeg_next::software::scaling::Flags::BILINEAR,
        )?;
        info!("swscale BGRA->YUV420P context created");

        Ok(Self {
            encoder,
            scaler,
            bgra_frame: ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::BGRA, width, height),
            frame: ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, width, height),
            packet: ffmpeg_next::Packet::empty(),
        })
    }
}

fn apply_bitrate(
    encoder: &mut ffmpeg_next::codec::encoder::video::Encoder,
    bitrate_bps: u32,
) -> Result<(), ffmpeg_next::Error> {
    let bitrate = bitrate_bps.max(1);
    encoder.set_bit_rate(bitrate as usize);
    encoder.set_max_bit_rate(bitrate as usize);
    encoder.set_tolerance((bitrate / 2) as usize);
    Ok(())
}

/// Check if an ffmpeg error is EAGAIN (no output available yet).
fn is_eagain(e: &ffmpeg_next::Error) -> bool {
    matches!(e, ffmpeg_next::Error::Other { errno } if *errno == libc::EAGAIN)
}
