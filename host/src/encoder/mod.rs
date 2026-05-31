use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
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
const AMF_ENCODER_NAME: &str = "h264_amf";
const AMF_CAPTURE_PACKET_LIMIT: u64 = 120;
const AMF_IDR_WARNING_PACKET: u64 = 60;
const AMF_FORCED_INTRA_PERIOD: u64 = 30;

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
    // Apply GUI encoder override at pipeline start. The override is consulted only here;
    // mid-stream changes don't take effect until the user requests a Restart.
    let (mut encoder_name, mut codec_display_name) = {
        let override_name = shared.encoder_override.lock().clone();
        match override_name {
            Some(name) if ffmpeg_next::encoder::find_by_name(&name).is_some() => {
                let display = match name.as_str() {
                    "h264_nvenc" => "H.264 (NVENC)",
                    "h264_amf" => "H.264 (AMF)",
                    "h264_qsv" => "H.264 (QSV)",
                    "libx264" => "H.264 (x264)",
                    other => {
                        warn!(encoder = %other, "Unknown encoder override label");
                        other
                    }
                };
                let display = display.to_string();
                info!(encoder = %name, "Encoder override honoured");
                (name, display)
            }
            Some(name) => {
                warn!(
                    encoder = %name,
                    "Encoder override not available in FFmpeg — falling back to auto-detected encoder"
                );
                (gpu.encoder_name.clone(), gpu.codec_display_name.clone())
            }
            None => (gpu.encoder_name.clone(), gpu.codec_display_name.clone()),
        }
    };

    if ffmpeg_next::encoder::find_by_name(&encoder_name).is_none() {
        return Err(format!("{} codec not found in FFmpeg", encoder_name).into());
    }
    info!(encoder = %encoder_name, "Found encoder codec");
    {
        let mut stats = PIPELINE_STATS.lock();
        stats.set_codec_name(&codec_display_name);
        // Fresh pipeline start: clear any stale software-fallback flag. The encode loop
        // re-sets it below only if a hardware encoder actually fails to open. A deliberate
        // libx264 override is not a fallback, so it intentionally leaves this false.
        stats.set_software_fallback(false);
    }

    let mut encoder_state: Option<EncoderState> = None;
    let mut rx = rx;
    let mut frames_since_last_idr: u64 = 0;

    // Guarantee the very first frame of every pipeline is an IDR for all encoders, so the
    // iPad never sits in `waitingForSyncSample` after a connect or pipeline restart.
    shared.force_next_idr.store(true, Ordering::SeqCst);

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
            match EncoderState::new(&encoder_name, raw_frame.width, raw_frame.height) {
                Ok(state) => encoder_state = Some(state),
                Err(e) if encoder_name != "libx264" => {
                    // A hardware encoder (commonly AMF) can fail to *open* even though it is
                    // compiled into FFmpeg — driver hiccup, AMF runtime busy/missing, transient
                    // device loss. Fall back to software once so streaming keeps a picture
                    // instead of the whole encode thread dying.
                    warn!(
                        encoder = %encoder_name,
                        error = %e,
                        "Encoder failed to open — falling back to libx264 (software)"
                    );
                    encoder_name = "libx264".to_string();
                    codec_display_name = "H.264 (x264)".to_string();
                    {
                        let mut stats = PIPELINE_STATS.lock();
                        stats.set_codec_name(&codec_display_name);
                        stats.set_software_fallback(true);
                    }
                    encoder_state =
                        Some(EncoderState::new(&encoder_name, raw_frame.width, raw_frame.height)?);
                }
                Err(e) => return Err(e),
            }
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
        let force_idr = shared.force_next_idr.swap(false, Ordering::SeqCst);
        let _forced_intra = prepare_frame_for_encode(
            &mut encoder.frame,
            raw_frame.frame_number,
            &encoder_name,
            &mut frames_since_last_idr,
            force_idr,
        );

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
                        &encoder_name,
                    );
                    let intra_only_access_unit = access_unit_is_intra_only(&nal_data);
                    let is_keyframe = packet_is_key
                        || contains_nal_type(&nal_data, 5)
                        || (is_amf_encoder(&encoder_name) && intra_only_access_unit);
                    encoder.observe_amf_diagnostics(
                        &nal_data,
                        raw_frame.frame_number,
                        packet_is_key,
                        is_keyframe,
                    );

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
    amf_diagnostics: Option<AmfBitstreamDiagnostics>,
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
        configure_encoder_flags(&mut encoder, encoder_name);

        let opts = encoder_options(encoder_name);

        let encoder = encoder.open_with(opts)?;
        log_encoder_configuration(encoder_name, &encoder);
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
            amf_diagnostics: AmfBitstreamDiagnostics::new(encoder_name),
        })
    }

    fn observe_amf_diagnostics(
        &mut self,
        normalized_packet: &[u8],
        sequence: u64,
        packet_is_key: bool,
        is_keyframe: bool,
    ) {
        if let Some(diagnostics) = self.amf_diagnostics.as_mut() {
            diagnostics.observe_packet(normalized_packet, sequence, packet_is_key, is_keyframe);
        }
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
            opts.set("usage", "lowlatency");
            opts.set("latency", "1");
            opts.set("quality", "speed");
            opts.set("profile", "constrained_baseline");
            // Level must cover the actual frame complexity or strict decoders (Apple
            // VideoToolbox) reject the stream even though lenient ones (ffmpeg) accept it.
            // 4.1 only covers ~1080p30; 1080p60 needs 4.2. Declare 5.1 for headroom across
            // resolutions — over-declaring a level is harmless, under-declaring is fatal.
            // NVENC works on iPad precisely because it auto-selects an adequate level.
            opts.set("level", "5.1");
            opts.set("coder", "cavlc");
            // No AUD (NAL 9): the iPad decoder strips AUDs before submitting to VideoToolbox,
            // so emitting them only wastes bytes. One FramePacket == one access unit, so the
            // receiver doesn't need AUDs for access-unit boundary detection.
            opts.set("aud", "0");
            // Emit SPS/PPS inline alongside each IDR; the host also prepends cached parameter
            // sets on every IDR in normalize_h264_payload as a belt-and-suspenders measure.
            opts.set("header_spacing", "1");
            opts.set("rc", "cbr");
            // IDR cadence is driven by encoder.set_gop(30) (mapped to AMF's IDR period). The
            // AMF_FORCED_INTRA_PERIOD safety net forces an I-frame every 30 frames in case the
            // wrapper doesn't honour gop in low-latency mode. Verify on-device via the AMF
            // packet logs that forced/periodic keyframes carry NAL 5 + SPS/PPS, not bare I.
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

fn prepare_frame_for_encode(
    frame: &mut ffmpeg_next::frame::Video,
    frame_number: u64,
    encoder_name: &str,
    frames_since_last_idr: &mut u64,
    force_next_idr: bool,
) -> bool {
    let is_amf = is_amf_encoder(encoder_name);
    // Force the next encoded frame to be an IDR whenever the transport flagged a
    // re-handshake. Applies to all encoders so any reconnect recovers within 1 frame.
    let force_hit = force_next_idr;
    // AMF needs an explicit periodic IDR to keep VideoToolbox in sync; other encoders
    // already produce a healthy keyframe cadence on their own.
    let period_hit = is_amf && *frames_since_last_idr >= AMF_FORCED_INTRA_PERIOD;
    let request_intra = period_hit || force_hit;

    frame.set_kind(if request_intra {
        ffmpeg_next::util::picture::Type::I
    } else {
        ffmpeg_next::util::picture::Type::None
    });

    unsafe {
        (*frame.as_mut_ptr()).key_frame = if request_intra { 1 } else { 0 };
    }

    if request_intra {
        *frames_since_last_idr = 0;
        let reason = if force_hit { "force_next_idr" } else { "period" };
        if is_amf {
            info!(
                encoder = encoder_name,
                frame = frame_number,
                period = AMF_FORCED_INTRA_PERIOD,
                reason,
                "[AMF] Forced IDR"
            );
        } else {
            info!(
                encoder = encoder_name,
                frame = frame_number,
                reason,
                "[encoder] Forced IDR"
            );
        }
    } else if is_amf {
        *frames_since_last_idr += 1;
    }

    request_intra
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
    amf_packet_logs: usize,
    /// Counts packets through normalize; used for AMF first-packet safeguards.
    packet_count: u64,
}

struct AmfBitstreamDiagnostics {
    capture_path: PathBuf,
    capture_file: Option<File>,
    total_packets: u64,
    captured_packets: u64,
    seen_random_access: bool,
    seen_idr: bool,
    warned_missing_random_access: bool,
    validation_complete: bool,
}

impl AmfBitstreamDiagnostics {
    fn new(encoder_name: &str) -> Option<Self> {
        if !is_amf_encoder(encoder_name) {
            return None;
        }

        let capture_path = diagnostic_capture_path();
        if let Some(parent) = capture_path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                warn!(
                    encoder = encoder_name,
                    path = %capture_path.display(),
                    error = %error,
                    "Failed to create AMF diagnostic directory"
                );
            }
        }

        let capture_file = match File::create(&capture_path) {
            Ok(file) => {
                info!(
                    encoder = encoder_name,
                    path = %capture_path.display(),
                    packet_limit = AMF_CAPTURE_PACKET_LIMIT,
                    "Capturing normalized AMF packets for diagnostics"
                );
                Some(file)
            }
            Err(error) => {
                warn!(
                    encoder = encoder_name,
                    path = %capture_path.display(),
                    error = %error,
                    "Failed to create AMF diagnostic capture file"
                );
                None
            }
        };

        Some(Self {
            capture_path,
            capture_file,
            total_packets: 0,
            captured_packets: 0,
            seen_random_access: false,
            seen_idr: false,
            warned_missing_random_access: false,
            validation_complete: false,
        })
    }

    fn observe_packet(
        &mut self,
        normalized_packet: &[u8],
        sequence: u64,
        packet_is_key: bool,
        is_keyframe: bool,
    ) {
        self.total_packets += 1;
        let contains_idr = contains_nal_type(normalized_packet, 5);
        if is_keyframe && !self.seen_random_access {
            self.seen_random_access = true;
            info!(
                sequence,
                packet_index = self.total_packets,
                packet_is_key,
                contains_idr,
                "Observed first AMF random-access access unit after normalization"
            );
        }
        if contains_idr && !self.seen_idr {
            self.seen_idr = true;
            info!(
                sequence,
                packet_index = self.total_packets,
                packet_is_key,
                is_keyframe,
                "Observed first AMF IDR after normalization"
            );
        }

        if !self.seen_random_access
            && !self.warned_missing_random_access
            && self.total_packets >= AMF_IDR_WARNING_PACKET
        {
            warn!(
                packet_index = self.total_packets,
                sequence,
                capture_path = %self.capture_path.display(),
                "AMF has not emitted any random-access access unit by packet 60; decoder startup may still fail"
            );
            self.warned_missing_random_access = true;
        }

        if self.captured_packets < AMF_CAPTURE_PACKET_LIMIT {
            if let Some(file) = self.capture_file.as_mut() {
                if let Err(error) = file.write_all(normalized_packet) {
                    warn!(
                        path = %self.capture_path.display(),
                        error = %error,
                        "Failed to append normalized AMF packet to diagnostic capture"
                    );
                    self.capture_file = None;
                }
            }

            self.captured_packets += 1;
            if self.captured_packets == AMF_CAPTURE_PACKET_LIMIT {
                if let Some(file) = self.capture_file.as_mut() {
                    if let Err(error) = file.flush() {
                        warn!(
                            path = %self.capture_path.display(),
                            error = %error,
                            "Failed to flush AMF diagnostic capture file"
                        );
                    }
                }
                self.capture_file = None;
                self.validate_capture_with_ffmpeg();
            }
        }
    }

    fn validate_capture_with_ffmpeg(&mut self) {
        if self.validation_complete {
            return;
        }
        self.validation_complete = true;

        let Some(ffmpeg_path) = find_ffmpeg_exe() else {
            warn!(
                path = %self.capture_path.display(),
                "AMF diagnostic capture completed but ffmpeg.exe was not found for software decode validation"
            );
            return;
        };

        match Command::new(&ffmpeg_path)
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-i",
                self.capture_path.to_string_lossy().as_ref(),
                "-f",
                "null",
                "-",
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                info!(
                    capture = %self.capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    "Local FFmpeg software decode of captured AMF bitstream succeeded"
                );
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(
                    capture = %self.capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    status = ?output.status.code(),
                    stderr = %stderr.trim(),
                    "Local FFmpeg software decode of captured AMF bitstream failed"
                );
            }
            Err(error) => {
                warn!(
                    capture = %self.capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    error = %error,
                    "Failed to launch ffmpeg.exe for AMF software decode validation"
                );
            }
        }
    }
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

    /// Refresh cached SPS/PPS from inline NAL units. Always overwrites so the cache
    /// never goes stale if the encoder emits a new parameter set mid-stream.
    fn update_parameter_sets_from_units(&mut self, units: &[Vec<u8>]) {
        for unit in units {
            match nal_type(unit) {
                Some(7) => self.sps = Some(unit.clone()),
                Some(8) => self.pps = Some(unit.clone()),
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
    if is_amf_encoder(encoder_name) && state.amf_packet_logs < 10 {
        let nal_types: Vec<String> = parsed
            .units
            .iter()
            .filter_map(|unit| nal_type(unit).map(|kind| kind.to_string()))
            .collect();
        info!(
            encoder = encoder_name,
            packet_index = state.amf_packet_logs + 1,
            input = %parsed.format,
            packet_is_key,
            packet_bytes = data.len(),
            packet_prefix = %hex_prefix(data, 16),
            nal_types = %nal_types.join(","),
            "Observed AMF encoded H.264 packet"
        );
        state.amf_packet_logs += 1;
    }

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
    let is_amf = is_amf_encoder(encoder_name);

    // For AMF, prepend cached SPS/PPS on every random-access access unit, even if the encoder
    // already emitted them inline. iPad VideoToolbox is sensitive to GOP-boundary parameter
    // freshness; the iPad now tears down its decoder on every IDR so redundant SPS/PPS are safe.
    // For other encoders, only prepend if the keyframe was missing parameter sets.
    let amf_startup_inject = is_amf && (!has_sps || !has_pps) && state.packet_count == 0;
    let amf_idr_redundant_prepend = is_amf && (packet_is_key || contains_idr);
    state.packet_count += 1;

    let should_prefix_parameter_sets = amf_idr_redundant_prepend
        || amf_startup_inject
        || ((packet_is_key || contains_idr) && (!has_sps || !has_pps));

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

    // Whenever we prepended cached SPS/PPS, drop any inline SPS(7)/PPS(8) NAL units so the
    // access unit doesn't contain duplicates.
    let drop_inline_parameter_sets = should_prefix_parameter_sets
        && matches!(
            (state.sps.as_deref(), state.pps.as_deref()),
            (Some(_), Some(_))
        );

    for unit in &parsed.units {
        if drop_inline_parameter_sets
            && matches!(nal_type(unit), Some(7) | Some(8))
        {
            continue;
        }
        append_annex_b_unit(&mut output, unit);
    }

    // Hex-dump first 8 bytes of every IDR-bearing AMF packet for runtime diagnostics.
    if is_amf && (packet_is_key || contains_idr) {
        tracing::debug!(
            encoder = encoder_name,
            packet_is_key,
            contains_idr,
            hex_prefix = %hex_prefix(&output, 8),
            "[AMF] IDR packet emitted"
        );
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

fn configure_encoder_flags(
    encoder: &mut ffmpeg_next::codec::encoder::video::Video,
    encoder_name: &str,
) {
    if !is_amf_encoder(encoder_name) {
        return;
    }

    let mut flags = codec_flags(encoder);
    flags.remove(ffmpeg_next::codec::Flags::GLOBAL_HEADER);
    flags.insert(ffmpeg_next::codec::Flags::CLOSED_GOP);
    encoder.set_flags(flags);

    info!(
        encoder = encoder_name,
        global_header = flags.contains(ffmpeg_next::codec::Flags::GLOBAL_HEADER),
        closed_gop = flags.contains(ffmpeg_next::codec::Flags::CLOSED_GOP),
        "Applied AMD-specific encoder flags"
    );
}

fn log_encoder_configuration(
    encoder_name: &str,
    encoder: &ffmpeg_next::codec::encoder::video::Encoder,
) {
    let flags = codec_flags(encoder);
    let extradata = encoder_extradata(encoder);
    let extradata_bytes = extradata.as_ref().map_or(0, Vec::len);
    let extradata_format = extradata
        .as_deref()
        .map(describe_extradata_format)
        .unwrap_or("None");
    let (sps_bytes, pps_bytes) = extradata
        .as_deref()
        .and_then(parameter_sets_from_extradata)
        .map(|(sps, pps)| (sps.len(), pps.len()))
        .unwrap_or((0, 0));
    let extradata_prefix = extradata
        .as_deref()
        .map(|bytes| hex_prefix(bytes, 16))
        .unwrap_or_else(|| "none".to_string());

    info!(
        encoder = encoder_name,
        global_header = flags.contains(ffmpeg_next::codec::Flags::GLOBAL_HEADER),
        closed_gop = flags.contains(ffmpeg_next::codec::Flags::CLOSED_GOP),
        extradata_bytes,
        extradata_format,
        sps_bytes,
        pps_bytes,
        extradata_prefix = %extradata_prefix,
        "Encoder H.264 configuration"
    );
}

fn codec_flags<T: AsRef<ffmpeg_next::codec::Context>>(context: &T) -> ffmpeg_next::codec::Flags {
    unsafe {
        let raw_flags = (*context.as_ref().as_ptr()).flags;
        ffmpeg_next::codec::Flags::from_bits_truncate(raw_flags as u32)
    }
}

fn describe_extradata_format(extradata: &[u8]) -> &'static str {
    if extradata.is_empty() {
        "None"
    } else if extradata.first().copied() == Some(1) {
        "AVCC"
    } else if !parse_annex_b_units(extradata).is_empty() {
        "AnnexB"
    } else {
        "Unknown"
    }
}

fn hex_prefix(data: &[u8], max_len: usize) -> String {
    data.iter()
        .take(max_len)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_amf_encoder(encoder_name: &str) -> bool {
    encoder_name == AMF_ENCODER_NAME
}

fn diagnostic_capture_path() -> PathBuf {
    diagnostic_dir().join("amf-first-120-packets.h264")
}

fn diagnostic_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("diagnostics");
        }
    }
    PathBuf::from("diagnostics")
}

fn find_ffmpeg_exe() -> Option<PathBuf> {
    let bundled = diagnostic_dir()
        .parent()
        .map(|dir| dir.join("ffmpeg.exe"))
        .filter(|path| path.is_file());
    if bundled.is_some() {
        return bundled;
    }

    std::env::var_os("FFMPEG_DIR")
        .map(PathBuf::from)
        .map(|dir| dir.join("bin").join("ffmpeg.exe"))
        .filter(|path| path.is_file())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum H264SliceKind {
    P,
    B,
    I,
    SP,
    SI,
}

fn nal_type(unit: &[u8]) -> Option<u8> {
    unit.first().map(|byte| byte & 0x1F)
}

fn contains_nal_type(data: &[u8], target: u8) -> bool {
    parse_annex_b_units(data)
        .iter()
        .any(|unit| nal_type(unit) == Some(target))
}

fn access_unit_is_intra_only(data: &[u8]) -> bool {
    let units = parse_annex_b_units(data);
    let mut saw_vcl = false;

    for unit in &units {
        match nal_type(unit) {
            Some(5) => saw_vcl = true,
            Some(1..=4) => {
                saw_vcl = true;
                let Some(slice_kind) = slice_kind(unit) else {
                    return false;
                };
                if !matches!(slice_kind, H264SliceKind::I | H264SliceKind::SI) {
                    return false;
                }
            }
            _ => {}
        }
    }

    saw_vcl
}

fn slice_kind(unit: &[u8]) -> Option<H264SliceKind> {
    match nal_type(unit)? {
        5 => Some(H264SliceKind::I),
        1..=4 => {
            let rbsp = rbsp_from_ebsp(unit.get(1..)?);
            let mut bits = BitReader::new(&rbsp);
            let _first_mb_in_slice = bits.read_ue()?;
            let slice_type = bits.read_ue()? % 5;
            match slice_type {
                0 => Some(H264SliceKind::P),
                1 => Some(H264SliceKind::B),
                2 => Some(H264SliceKind::I),
                3 => Some(H264SliceKind::SP),
                4 => Some(H264SliceKind::SI),
                _ => None,
            }
        }
        _ => None,
    }
}

fn rbsp_from_ebsp(data: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(data.len());
    let mut zero_count = 0usize;

    for &byte in data {
        if zero_count >= 2 && byte == 0x03 {
            zero_count = 0;
            continue;
        }

        rbsp.push(byte);
        if byte == 0 {
            zero_count += 1;
        } else {
            zero_count = 0;
        }
    }

    rbsp
}

struct BitReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.bit_offset / 8)?;
        let shift = 7 - (self.bit_offset % 8);
        self.bit_offset += 1;
        Some((byte >> shift) & 1)
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0usize;
        while self.read_bit()? == 0 {
            leading_zero_bits += 1;
            if leading_zero_bits >= 32 {
                return None;
            }
        }

        let suffix = if leading_zero_bits == 0 {
            0
        } else {
            self.read_bits(leading_zero_bits)?
        };
        Some(((1u32 << leading_zero_bits) - 1) + suffix)
    }
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

    #[test]
    fn amf_startup_injection_fills_missing_parameter_set_when_only_pps_is_inline() {
        let extradata = [
            1, 66, 0, 30, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x02, 0x68,
            0xCE,
        ];
        let payload = [0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x41, 0x9A, 0x22];

        let mut state = H264BitstreamState::default();
        state.refresh_parameter_sets_from_extradata(Some(extradata.to_vec()));

        let normalized = normalize_h264_payload(&payload, false, &mut state, "h264_amf");
        assert_eq!(
            normalized,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x41,
                0x9A, 0x22
            ]
        );
    }

    #[test]
    fn amf_does_not_reinject_parameter_sets_mid_stream_on_inter_frame() {
        let extradata = [
            1, 66, 0, 30, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x02, 0x68,
            0xCE,
        ];
        let payload = [0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x41, 0x9A, 0x22];

        let mut state = H264BitstreamState::default();
        state.refresh_parameter_sets_from_extradata(Some(extradata.to_vec()));
        state.packet_count = 30;

        let normalized = normalize_h264_payload(&payload, false, &mut state, "h264_amf");
        assert_eq!(normalized, payload);
    }

    #[test]
    fn amf_reinjects_parameter_sets_on_every_idr_even_mid_stream() {
        let extradata = [
            1, 66, 0, 30, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x02, 0x68,
            0xCE,
        ];
        let packet = [0x00, 0x00, 0x00, 0x03, 0x65, 0x88, 0x84];

        let mut state = H264BitstreamState::default();
        state.refresh_parameter_sets_from_extradata(Some(extradata.to_vec()));
        state.packet_count = 200;

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
    fn amf_drops_inline_parameter_sets_when_redundantly_prepending() {
        let extradata = [
            1, 66, 0, 30, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x02, 0x68,
            0xCE,
        ];
        // Inline SPS + PPS + IDR. AMF must prepend cached SPS/PPS and drop the inline duplicates.
        let payload = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88,
            0x84,
        ];

        let mut state = H264BitstreamState::default();
        state.refresh_parameter_sets_from_extradata(Some(extradata.to_vec()));
        state.packet_count = 50;

        let normalized = normalize_h264_payload(&payload, true, &mut state, "h264_amf");
        // Expect a single SPS + PPS pair followed by IDR — no duplicates.
        assert_eq!(
            normalized,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88,
                0x84
            ]
        );
    }

    #[test]
    fn nvenc_keyframe_with_inline_sps_pps_is_passed_through_unchanged() {
        // NVENC path must not be altered by the AMF redundant-prepend behaviour.
        let payload = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88,
            0x84,
        ];
        let mut state = H264BitstreamState::default();
        let normalized = normalize_h264_payload(&payload, true, &mut state, "h264_nvenc");
        assert_eq!(normalized, payload);
    }

    #[test]
    fn detects_non_idr_intra_access_unit_as_random_access() {
        let payload = [0, 0, 0, 1, 0x09, 0x30, 0, 0, 0, 1, 0x41, 0xB8];

        assert!(access_unit_is_intra_only(&payload));
    }

    #[test]
    fn rejects_predicted_access_unit_as_random_access() {
        let payload = [0, 0, 0, 1, 0x09, 0x30, 0, 0, 0, 1, 0x41, 0xE0];

        assert!(!access_unit_is_intra_only(&payload));
    }
}
