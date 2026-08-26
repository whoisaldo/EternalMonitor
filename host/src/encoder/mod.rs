use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::slice;
use std::sync::atomic::Ordering;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use eternal_wire::h264::{
    contains_nal_type, describe_extradata_format, hex_prefix, is_amf_encoder,
    normalize_h264_payload_with_info, parameter_sets_from_extradata, H264BitstreamState,
};

use crate::capture::FrameSlot;
use crate::control::SharedControl;
use crate::gpu::GpuInfo;
use crate::stats::PIPELINE_STATS;

const CHANNEL_CAPACITY: usize = 4;
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

/// Runs the H.264 encode loop on the CURRENT thread until the capture slot
/// closes or stop is requested. The supervisor spawns and monitors this.
pub fn run_encode_stage(
    frames: FrameSlot,
    tx: mpsc::Sender<NALUnit>,
    shared: SharedControl,
    gpu: GpuInfo,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let result = run_encode_loop(frames, tx, shared, gpu);
    if let Err(ref e) = result {
        error!(error = %e, "Encode loop exited with error");
    }
    result
}

/// Capacity of the encoded-output channel (lossless: encoded frames are never
/// dropped — a lost P-frame corrupts until the next IDR).
pub const NAL_CHANNEL_CAPACITY: usize = CHANNEL_CAPACITY;

fn run_encode_loop(
    frames: FrameSlot,
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
    let mut frames_since_last_idr: u64 = 0;
    // Bounded retries for the AMF startup case where a keyframe is emitted before any SPS/PPS are
    // available (empty extradata + no inline parameter sets). We nudge another IDR a few times so
    // a subsequent keyframe carrying inline parameter sets can recover, instead of leaving the
    // iPad unable to build a format description.
    let mut amf_startup_idr_retries: u8 = 0;
    // Suppresses reopen retry storms after a failed bitrate change.
    let mut failed_reopen_bitrate: Option<u32> = None;

    // Guarantee the very first frame of every pipeline is an IDR for all encoders, so the
    // iPad never sits in `waitingForSyncSample` after a connect or pipeline restart.
    shared.force_next_idr.store(true, Ordering::SeqCst);

    // Test-harness fault injection: die after N encoded frames so the E2E can
    // prove the supervisor's auto-restart. Consumed once (self-clearing) so
    // the restarted generation streams normally.
    let fault_after: Option<u64> = std::env::var("ETERNAL_FAULT_ENCODER_AFTER")
        .ok()
        .and_then(|v| v.trim().parse().ok());

    while let Some(raw_frame) = frames.blocking_take() {
        if !shared.running.load(Ordering::SeqCst) {
            info!("Encoder loop stopping on running=false");
            break;
        }

        if let Some(after) = fault_after {
            if raw_frame.frame_number >= after {
                std::env::remove_var("ETERNAL_FAULT_ENCODER_AFTER");
                return Err("injected encoder fault (ETERNAL_FAULT_ENCODER_AFTER)".into());
            }
        }

        if raw_frame.data.is_empty() {
            warn!(
                frame = raw_frame.frame_number,
                "Skipping frame with empty data"
            );
            continue;
        }

        let desired_bitrate = shared.abr_current_bps.load(Ordering::SeqCst).max(500_000);
        let target_fps = shared.target_fps.load(Ordering::SeqCst);

        if encoder_state.is_none() {
            match EncoderState::new(
                &encoder_name,
                raw_frame.width,
                raw_frame.height,
                desired_bitrate,
                target_fps,
            ) {
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
                    encoder_state = Some(EncoderState::new(
                        &encoder_name,
                        raw_frame.width,
                        raw_frame.height,
                        desired_bitrate,
                        target_fps,
                    )?);
                }
                Err(e) => return Err(e),
            }
        } else if encoder_state
            .as_ref()
            .is_some_and(|state| state.opened_bitrate != desired_bitrate)
            && failed_reopen_bitrate != Some(desired_bitrate)
        {
            // Bitrate changed (ABR rung or the user's slider): hardware
            // encoders ignore bitrate pokes on an open context, so a real
            // change means a session reopen (~50-200ms hiccup, no transport
            // teardown, same stream epoch) followed by a forced IDR.
            let previous = encoder_state
                .as_ref()
                .map(|s| s.opened_bitrate)
                .unwrap_or(0);
            match EncoderState::new(
                &encoder_name,
                raw_frame.width,
                raw_frame.height,
                desired_bitrate,
                target_fps,
            ) {
                Ok(state) => {
                    info!(
                        from = previous,
                        to = desired_bitrate,
                        "Reopened encoder session for new bitrate"
                    );
                    encoder_state = Some(state);
                    failed_reopen_bitrate = None;
                    shared.force_next_idr.store(true, Ordering::SeqCst);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        bitrate = desired_bitrate,
                        "Encoder reopen failed — keeping the current session"
                    );
                    failed_reopen_bitrate = Some(desired_bitrate);
                }
            }
        }

        let encoder = encoder_state
            .as_mut()
            .expect("Encoder state must be initialized");
        let current_bitrate = encoder.opened_bitrate;

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
        let pts = if encoder.legacy_pts {
            raw_frame.frame_number as i64
        } else {
            let epoch = *encoder.pts_epoch.get_or_insert(raw_frame.timestamp);
            let real = raw_frame
                .timestamp
                .saturating_duration_since(epoch)
                .as_micros() as i64;
            // Strict monotonicity (keepalive resends and clock quirks).
            let pts = real.max(encoder.last_pts + 1);
            encoder.last_pts = pts;
            pts
        };
        encoder.frame.set_pts(Some(pts));
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
                    let (nal_data, au_info) = normalize_h264_payload_with_info(
                        packet_bytes,
                        packet_is_key,
                        &mut encoder.h264,
                        &encoder_name,
                    );
                    let is_keyframe = packet_is_key
                        || au_info.contains_idr
                        || (is_amf_encoder(&encoder_name) && au_info.intra_only);
                    encoder.observe_amf_diagnostics(
                        &nal_data,
                        raw_frame.frame_number,
                        packet_is_key,
                        is_keyframe,
                    );

                    // AMF startup safety net: a keyframe went out but we still have no cached
                    // SPS/PPS (empty extradata and none inline), so the iPad can't build a format
                    // description. Nudge another IDR a few times — a later keyframe may carry the
                    // parameter sets inline and recover the stream.
                    if is_keyframe
                        && is_amf_encoder(&encoder_name)
                        && encoder.h264.needs_parameter_sets()
                        && amf_startup_idr_retries < 3
                    {
                        amf_startup_idr_retries += 1;
                        warn!(
                            retry = amf_startup_idr_retries,
                            "AMF keyframe still lacks SPS/PPS — forcing another IDR to recover"
                        );
                        shared.force_next_idr.store(true, Ordering::SeqCst);
                    }

                    PIPELINE_STATS
                        .lock()
                        .record_encode(encode_us, nal_data.len(), current_bitrate);
                    crate::capture::heartbeat(&shared.hb_encode_frame_ms);

                    debug!(
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
    /// The bitrate this session was opened with; a change requests a reopen.
    opened_bitrate: u32,
    /// First frame's capture instant — the PTS epoch for this session.
    pts_epoch: Option<std::time::Instant>,
    last_pts: i64,
    /// ETERNAL_LEGACY_PTS=1 restores the old dense frame-counter PTS at
    /// time_base 1/60 (escape hatch until the real-PTS path is verified on
    /// NVENC/AMF hardware).
    legacy_pts: bool,
}

impl EncoderState {
    fn new(
        encoder_name: &str,
        width: u32,
        height: u32,
        bitrate_bps: u32,
        target_fps: u32,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let legacy_pts = std::env::var("ETERNAL_LEGACY_PTS").is_ok_and(|v| v.trim() == "1");
        let codec = ffmpeg_next::encoder::find_by_name(encoder_name)
            .ok_or_else(|| format!("{} codec not found in FFmpeg", encoder_name))?;
        let context = ffmpeg_next::codec::Context::new_with_codec(codec);
        let mut encoder = context.encoder().video()?;

        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg_next::format::Pixel::YUV420P);
        if legacy_pts {
            encoder.set_time_base(ffmpeg_next::Rational(1, 60));
        } else {
            // Microsecond PTS from real capture times: rate control finally
            // sees the true cadence (idle keepalives, 30 fps modes) instead of
            // being told every frame is 1/60s apart.
            encoder.set_time_base(ffmpeg_next::Rational(1, 1_000_000));
            encoder.set_frame_rate(Some(ffmpeg_next::Rational(target_fps.max(1) as i32, 1)));
        }
        encoder.set_max_b_frames(0);
        encoder.set_bit_rate(bitrate_bps.max(500_000) as usize);
        encoder.set_gop(30);
        configure_encoder_flags(&mut encoder, encoder_name);

        let opts = encoder_options(encoder_name);

        let encoder = encoder.open_with(opts)?;
        log_encoder_configuration(encoder_name, &encoder);
        info!(
            width,
            height,
            bitrate = bitrate_bps,
            fps = target_fps,
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
            // 1:1 scale — the "filter" only converts colorspace, so take the fast path.
            ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
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
            opened_bitrate: bitrate_bps,
            pts_epoch: None,
            last_pts: -1,
            legacy_pts,
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
        let reason = if force_hit {
            "force_next_idr"
        } else {
            "period"
        };
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
        // Opt-in only: the capture writes every packet to disk and used to
        // stall the encode thread for seconds on the ffmpeg.exe validation.
        if !std::env::var("ETERNAL_AMF_DIAG").is_ok_and(|v| v.trim() == "1") {
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
                // Never on the encode thread: the ffmpeg.exe round trip takes
                // seconds and used to freeze the stream mid-session.
                let capture_path = self.capture_path.clone();
                std::thread::spawn(move || validate_capture_with_ffmpeg(&capture_path));
                self.validation_complete = true;
            }
        }
    }
}

fn validate_capture_with_ffmpeg(capture_path: &std::path::Path) {
    {
        let Some(ffmpeg_path) = find_ffmpeg_exe() else {
            warn!(
                path = %capture_path.display(),
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
                capture_path.to_string_lossy().as_ref(),
                "-f",
                "null",
                "-",
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                info!(
                    capture = %capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    "Local FFmpeg software decode of captured AMF bitstream succeeded"
                );
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(
                    capture = %capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    status = ?output.status.code(),
                    stderr = %stderr.trim(),
                    "Local FFmpeg software decode of captured AMF bitstream failed"
                );
            }
            Err(error) => {
                warn!(
                    capture = %capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    error = %error,
                    "Failed to launch ffmpeg.exe for AMF software decode validation"
                );
            }
        }
    }
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

fn diagnostic_capture_path() -> PathBuf {
    diagnostic_dir().join("amf-first-120-packets.h264")
}

fn diagnostic_dir() -> PathBuf {
    // Write under %APPDATA% so the capture works even when the app is installed read-only under
    // Program Files (the exe directory is not writable by the non-elevated host there).
    if let Some(dir) = crate::settings::app_data_dir() {
        return dir.join("diagnostics");
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("diagnostics")))
        .unwrap_or_else(|| PathBuf::from("diagnostics"))
}

fn find_ffmpeg_exe() -> Option<PathBuf> {
    // ffmpeg.exe is bundled next to the host exe; reading it is fine even under Program Files.
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ffmpeg.exe")))
        .filter(|path| path.is_file());
    if bundled.is_some() {
        return bundled;
    }

    std::env::var_os("FFMPEG_DIR")
        .map(PathBuf::from)
        .map(|dir| dir.join("bin").join("ffmpeg.exe"))
        .filter(|path| path.is_file())
}
