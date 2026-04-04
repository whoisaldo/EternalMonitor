use std::slice;
use std::sync::atomic::Ordering;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::capture::RawFrame;
use crate::control::SharedControl;
use crate::gpu::GpuInfo;
use crate::stats::PIPELINE_STATS;

const CHANNEL_CAPACITY: usize = 4;
const DEFAULT_BITRATE: u32 = 15_000_000;

/// Encoded H.264 NAL unit ready for transport.
#[derive(Debug, Clone)]
pub struct NALUnit {
    pub data: Vec<u8>,
    pub sequence: u64,
    pub timestamp: Instant,
    pub encode_duration_us: u128,
    pub is_keyframe: bool,
}

/// Starts the H.264 encode loop on a dedicated blocking thread.
/// Consumes `RawFrame`s from the capture stage and produces `NALUnit`s.
pub fn start_encoder(
    rx: mpsc::Receiver<RawFrame>,
    shared: SharedControl,
    gpu: GpuInfo,
) -> mpsc::Receiver<NALUnit> {
    let (tx, nal_rx) = mpsc::channel::<NALUnit>(CHANNEL_CAPACITY);

    tokio::task::spawn_blocking(move || {
        if let Err(e) = run_encode_loop(rx, tx, shared, gpu) {
            error!(error = %e, "Encode loop exited with error");
        }
    });

    nal_rx
}

fn run_encode_loop(
    rx: mpsc::Receiver<RawFrame>,
    tx: mpsc::Sender<NALUnit>,
    shared: SharedControl,
    gpu: GpuInfo,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if ffmpeg_next::encoder::find_by_name(&gpu.encoder_name).is_none() {
        return Err(format!("{} codec not found in FFmpeg", gpu.encoder_name).into());
    }
    info!(encoder = %gpu.encoder_name, "Found encoder codec");
    PIPELINE_STATS
        .lock()
        .set_codec_name(&gpu.codec_display_name);

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
            encoder_state = Some(EncoderState::new(
                &gpu.encoder_name,
                raw_frame.width,
                raw_frame.height,
            )?);
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
                    if encoder.h264.needs_parameter_sets() {
                        encoder
                            .h264
                            .refresh_parameter_sets_from_extradata(encoder_extradata(
                                &encoder.encoder,
                            ));
                    }

                    let packet_bytes = encoder.packet.data().unwrap_or_default();
                    let packet_is_key = encoder.packet.is_key();
                    let nal_data = normalize_h264_payload(
                        packet_bytes,
                        packet_is_key,
                        &mut encoder.h264,
                        &gpu.encoder_name,
                    );
                    let is_keyframe = packet_is_key || contains_nal_type(&nal_data, 5);

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
                        is_keyframe,
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
    h264: H264BitstreamState,
}

impl EncoderState {
    fn new(
        encoder_name: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let codec = ffmpeg_next::encoder::find_by_name(encoder_name)
            .ok_or_else(|| format!("{} codec not found in FFmpeg", encoder_name))?;
        let context = ffmpeg_next::codec::Context::new_with_codec(codec);
        let mut encoder = context.encoder().video()?;

        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg_next::format::Pixel::YUV420P);
        encoder.set_time_base(ffmpeg_next::Rational(1, 60));
        encoder.set_max_b_frames(0);
        encoder.set_bit_rate(DEFAULT_BITRATE as usize);
        encoder.set_gop(30);

        let opts = encoder_options(encoder_name);

        let encoder = encoder.open_with(opts)?;
        info!(
            width,
            height,
            bitrate = DEFAULT_BITRATE,
            encoder = encoder_name,
            "Encoder opened"
        );
        let mut h264 = H264BitstreamState::default();
        h264.refresh_parameter_sets_from_extradata(encoder_extradata(&encoder));

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
            bgra_frame: ffmpeg_next::frame::Video::new(
                ffmpeg_next::format::Pixel::BGRA,
                width,
                height,
            ),
            frame: ffmpeg_next::frame::Video::new(
                ffmpeg_next::format::Pixel::YUV420P,
                width,
                height,
            ),
            packet: ffmpeg_next::Packet::empty(),
            h264,
        })
    }
}

fn encoder_options(encoder_name: &str) -> ffmpeg_next::Dictionary<'_> {
    let mut opts = ffmpeg_next::Dictionary::new();
    match encoder_name {
        "h264_nvenc" => {
            opts.set("preset", "p1");
            opts.set("tune", "ll");
            opts.set("profile", "baseline");
            opts.set("zerolatency", "1");
            opts.set("rc", "cbr");
        }
        "h264_amf" => {
            opts.set("usage", "ultralowlatency");
            opts.set("quality", "speed");
            opts.set("profile", "constrained_baseline");
            opts.set("header_insertion_mode", "idr");
            opts.set("rc", "cbr");
        }
        "h264_qsv" => {
            opts.set("preset", "veryfast");
            opts.set("profile", "baseline");
            opts.set("look_ahead", "0");
        }
        "libx264" => {
            opts.set("preset", "ultrafast");
            opts.set("tune", "zerolatency");
            opts.set("profile", "baseline");
        }
        _ => {
            opts.set("profile", "baseline");
        }
    }
    opts
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

#[derive(Debug, Default)]
struct H264BitstreamState {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    logged_first_packet: bool,
    warned_missing_parameter_sets: bool,
}

impl H264BitstreamState {
    fn needs_parameter_sets(&self) -> bool {
        self.sps.is_none() || self.pps.is_none()
    }

    fn refresh_parameter_sets_from_extradata(&mut self, extradata: Option<Vec<u8>>) {
        let Some(extradata) = extradata else { return };
        let Some((sps, pps)) = parameter_sets_from_extradata(&extradata) else {
            return;
        };

        if self.sps.is_none() {
            self.sps = Some(sps);
        }
        if self.pps.is_none() {
            self.pps = Some(pps);
        }
    }

    fn update_parameter_sets_from_units(&mut self, units: &[Vec<u8>]) {
        for unit in units {
            match nal_type(unit) {
                Some(7) if self.sps.is_none() => self.sps = Some(unit.clone()),
                Some(8) if self.pps.is_none() => self.pps = Some(unit.clone()),
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum H264PacketFormat {
    AnnexB,
    Avcc(usize),
    Unknown,
}

impl std::fmt::Display for H264PacketFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AnnexB => write!(f, "AnnexB"),
            Self::Avcc(length_size) => write!(f, "AVCC(len={length_size})"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

struct ParsedBitstream {
    format: H264PacketFormat,
    units: Vec<Vec<u8>>,
}

fn normalize_h264_payload(
    data: &[u8],
    packet_is_key: bool,
    state: &mut H264BitstreamState,
    encoder_name: &str,
) -> Vec<u8> {
    let parsed = parse_h264_bitstream(data);
    if !state.logged_first_packet && !parsed.units.is_empty() {
        let nal_types: Vec<String> = parsed
            .units
            .iter()
            .filter_map(|unit| nal_type(unit).map(|kind| kind.to_string()))
            .collect();
        info!(
            encoder = encoder_name,
            input = %parsed.format,
            packet_is_key,
            nal_types = %nal_types.join(","),
            "Observed first encoded H.264 packet"
        );
        state.logged_first_packet = true;
    }

    if parsed.units.is_empty() {
        return data.to_vec();
    }

    state.update_parameter_sets_from_units(&parsed.units);

    let contains_idr = parsed.units.iter().any(|unit| nal_type(unit) == Some(5));
    let has_sps = parsed.units.iter().any(|unit| nal_type(unit) == Some(7));
    let has_pps = parsed.units.iter().any(|unit| nal_type(unit) == Some(8));
    let should_prefix_parameter_sets = (packet_is_key || contains_idr) && (!has_sps || !has_pps);

    let mut output = Vec::with_capacity(data.len() + 128);
    if should_prefix_parameter_sets {
        match (state.sps.as_deref(), state.pps.as_deref()) {
            (Some(sps), Some(pps)) => {
                append_annex_b_unit(&mut output, sps);
                append_annex_b_unit(&mut output, pps);
            }
            _ if !state.warned_missing_parameter_sets => {
                warn!(
                    encoder = encoder_name,
                    packet_is_key,
                    contains_idr,
                    "Keyframe packet is missing SPS/PPS and encoder extradata did not provide them"
                );
                state.warned_missing_parameter_sets = true;
            }
            _ => {}
        }
    }

    for unit in &parsed.units {
        append_annex_b_unit(&mut output, unit);
    }

    output
}

fn parse_h264_bitstream(data: &[u8]) -> ParsedBitstream {
    let annex_b_units = parse_annex_b_units(data);
    if !annex_b_units.is_empty() {
        return ParsedBitstream {
            format: H264PacketFormat::AnnexB,
            units: annex_b_units,
        };
    }

    for length_size in [4usize, 2, 1] {
        if let Some(units) = parse_length_prefixed_units(data, length_size) {
            return ParsedBitstream {
                format: H264PacketFormat::Avcc(length_size),
                units,
            };
        }
    }

    ParsedBitstream {
        format: H264PacketFormat::Unknown,
        units: Vec::new(),
    }
}

fn parse_annex_b_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut units = Vec::new();
    let Some((mut cursor, _)) = find_start_code(data, 0) else {
        return units;
    };

    while let Some((nal_start, start_code_len)) = find_start_code(data, cursor) {
        cursor = nal_start + start_code_len;
        let next_start = find_start_code(data, cursor)
            .map(|(offset, _)| offset)
            .unwrap_or(data.len());
        if next_start > cursor {
            let unit = trim_trailing_zeros(&data[cursor..next_start]);
            if !unit.is_empty() {
                units.push(unit.to_vec());
            }
        }
        if next_start >= data.len() {
            break;
        }
        cursor = next_start;
    }

    units
}

fn parse_length_prefixed_units(data: &[u8], length_size: usize) -> Option<Vec<Vec<u8>>> {
    if data.len() < length_size || !(1..=4).contains(&length_size) {
        return None;
    }

    let mut units = Vec::new();
    let mut cursor = 0usize;
    while cursor + length_size <= data.len() {
        let nal_len = read_be_length(&data[cursor..cursor + length_size])?;
        cursor += length_size;

        if nal_len == 0 || cursor + nal_len > data.len() {
            return None;
        }

        units.push(data[cursor..cursor + nal_len].to_vec());
        cursor += nal_len;
    }

    if cursor == data.len() && !units.is_empty() {
        Some(units)
    } else {
        None
    }
}

fn parameter_sets_from_extradata(extradata: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if extradata.is_empty() {
        return None;
    }

    if extradata.first().copied() == Some(1) {
        return parse_avcc_parameter_sets(extradata);
    }

    let units = parse_annex_b_units(extradata);
    let sps = units.iter().find(|unit| nal_type(unit) == Some(7))?.clone();
    let pps = units.iter().find(|unit| nal_type(unit) == Some(8))?.clone();
    Some((sps, pps))
}

fn parse_avcc_parameter_sets(extradata: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if extradata.len() < 7 {
        return None;
    }

    let mut cursor = 5usize;
    let sps_count = (extradata[cursor] & 0x1F) as usize;
    cursor += 1;

    let mut sps = None;
    for _ in 0..sps_count {
        if cursor + 2 > extradata.len() {
            return None;
        }
        let len = u16::from_be_bytes([extradata[cursor], extradata[cursor + 1]]) as usize;
        cursor += 2;
        if cursor + len > extradata.len() {
            return None;
        }
        if sps.is_none() {
            sps = Some(extradata[cursor..cursor + len].to_vec());
        }
        cursor += len;
    }

    if cursor >= extradata.len() {
        return None;
    }

    let pps_count = extradata[cursor] as usize;
    cursor += 1;

    let mut pps = None;
    for _ in 0..pps_count {
        if cursor + 2 > extradata.len() {
            return None;
        }
        let len = u16::from_be_bytes([extradata[cursor], extradata[cursor + 1]]) as usize;
        cursor += 2;
        if cursor + len > extradata.len() {
            return None;
        }
        if pps.is_none() {
            pps = Some(extradata[cursor..cursor + len].to_vec());
        }
        cursor += len;
    }

    Some((sps?, pps?))
}

fn encoder_extradata(encoder: &ffmpeg_next::codec::encoder::video::Encoder) -> Option<Vec<u8>> {
    unsafe {
        let context = encoder.as_ptr();
        let extradata = (*context).extradata;
        let extradata_size = (*context).extradata_size;
        if extradata.is_null() || extradata_size <= 0 {
            None
        } else {
            Some(slice::from_raw_parts(extradata, extradata_size as usize).to_vec())
        }
    }
}

fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if i + 2 < data.len() && data[i + 2] == 1 {
                return Some((i, 3));
            }
            if i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                return Some((i, 4));
            }
        }
        i += 1;
    }
    None
}

fn read_be_length(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }

    let mut value = 0usize;
    for byte in bytes {
        value = (value << 8) | usize::from(*byte);
    }
    Some(value)
}

fn trim_trailing_zeros(data: &[u8]) -> &[u8] {
    let mut end = data.len();
    while end > 0 && data[end - 1] == 0 {
        end -= 1;
    }
    &data[..end]
}

fn append_annex_b_unit(output: &mut Vec<u8>, unit: &[u8]) {
    output.extend_from_slice(&[0, 0, 0, 1]);
    output.extend_from_slice(unit);
}

fn nal_type(unit: &[u8]) -> Option<u8> {
    unit.first().map(|byte| byte & 0x1F)
}

fn contains_nal_type(data: &[u8], target: u8) -> bool {
    parse_annex_b_units(data)
        .iter()
        .any(|unit| nal_type(unit) == Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_parameter_sets_from_avcc_extradata() {
        let extradata = [
            1, 66, 0, 30, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x02, 0x68,
            0xCE,
        ];

        let (sps, pps) = parameter_sets_from_extradata(&extradata).expect("parameter sets");
        assert_eq!(sps, vec![0x67, 0x42, 0x00, 0x1E]);
        assert_eq!(pps, vec![0x68, 0xCE]);
    }

    #[test]
    fn converts_avcc_keyframe_to_annex_b_and_prefixes_cached_parameter_sets() {
        let extradata = [
            1, 66, 0, 30, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x02, 0x68,
            0xCE,
        ];
        let packet = [0x00, 0x00, 0x00, 0x03, 0x65, 0x88, 0x84];

        let mut state = H264BitstreamState::default();
        state.refresh_parameter_sets_from_extradata(Some(extradata.to_vec()));

        let normalized = normalize_h264_payload(&packet, true, &mut state, "h264_amf");
        assert_eq!(
            normalized,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88,
                0x84
            ]
        );
    }

    #[test]
    fn preserves_annex_b_payload_that_already_contains_parameter_sets() {
        let payload = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88,
            0x84,
        ];

        let mut state = H264BitstreamState::default();
        let normalized = normalize_h264_payload(&payload, true, &mut state, "h264_nvenc");

        assert_eq!(normalized, payload);
    }
}
