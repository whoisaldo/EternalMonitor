//! Pure H.264 bitstream logic shared by the host encoder pipeline and tests.
//!
//! Moved verbatim from `host/src/encoder/mod.rs` so it can compile and test on
//! every platform (no ffmpeg, no Win32). Behavior is load-bearing for the
//! iPad's VideoToolbox decoder — especially the AMF parameter-set injection
//! rules — so changes here must keep the test vectors below green.

use tracing::{info, warn};

/// FFmpeg codec name of the AMD hardware encoder, which needs special
/// parameter-set handling throughout this module.
pub const AMF_ENCODER_NAME: &str = "h264_amf";

/// Cached SPS/PPS plus normalization counters carried across packets of one
/// encoder session.
#[derive(Debug, Default)]
pub struct H264BitstreamState {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    logged_first_packet: bool,
    warned_missing_parameter_sets: bool,
    amf_packet_logs: usize,
    /// Counts packets through normalize; used for AMF first-packet safeguards.
    packet_count: u64,
}

impl H264BitstreamState {
    pub fn needs_parameter_sets(&self) -> bool {
        self.sps.is_none() || self.pps.is_none()
    }

    pub fn refresh_parameter_sets_from_extradata(&mut self, extradata: Option<Vec<u8>>) {
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

/// Rewrites one encoded packet into clean Annex B, injecting cached SPS/PPS
/// according to the per-encoder rules the iPad decoder depends on.
pub fn normalize_h264_payload(
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
    // AMF's forced periodic intra (every AMF_FORCED_INTRA_PERIOD frames) can come back as an
    // all-I-slice access unit that is NOT an IDR — NAL type 1, not 5 — so `packet_is_key` and
    // `contains_idr` are both false. The iPad still treats an intra-only access unit as a
    // random-access point and recreates its decoder on it, so it must carry SPS/PPS too;
    // otherwise a decoder that lost its parameter sets can never resync on these frames.
    let intra_only = is_amf && units_are_intra_only(&parsed.units);

    // For AMF, prepend cached SPS/PPS on every random-access access unit, even if the encoder
    // already emitted them inline. iPad VideoToolbox is sensitive to GOP-boundary parameter
    // freshness; the iPad now tears down its decoder on every IDR so redundant SPS/PPS are safe.
    // For other encoders, only prepend if the keyframe was missing parameter sets.
    let amf_startup_inject = is_amf && (!has_sps || !has_pps) && state.packet_count == 0;
    let amf_idr_redundant_prepend = is_amf && (packet_is_key || contains_idr || intra_only);
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
        if drop_inline_parameter_sets && matches!(nal_type(unit), Some(7) | Some(8)) {
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

/// Splits an Annex B byte stream into NAL units (start codes removed).
pub fn parse_annex_b_units(data: &[u8]) -> Vec<Vec<u8>> {
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

/// Extracts (SPS, PPS) from encoder extradata in either AVCC or Annex B form.
pub fn parameter_sets_from_extradata(extradata: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
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

/// Human-readable classification of encoder extradata bytes, for logging.
pub fn describe_extradata_format(extradata: &[u8]) -> &'static str {
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

/// First `max_len` bytes as spaced uppercase hex, for logging.
pub fn hex_prefix(data: &[u8], max_len: usize) -> String {
    data.iter()
        .take(max_len)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_amf_encoder(encoder_name: &str) -> bool {
    encoder_name == AMF_ENCODER_NAME
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

/// NAL type of a unit whose start code has been removed.
pub fn nal_type(unit: &[u8]) -> Option<u8> {
    unit.first().map(|byte| byte & 0x1F)
}

/// True if the Annex B stream contains a NAL unit of the given type.
pub fn contains_nal_type(data: &[u8], target: u8) -> bool {
    parse_annex_b_units(data)
        .iter()
        .any(|unit| nal_type(unit) == Some(target))
}

/// True if the Annex B access unit is a random-access point even without an
/// IDR NAL (see [`units_are_intra_only`]).
pub fn access_unit_is_intra_only(data: &[u8]) -> bool {
    units_are_intra_only(&parse_annex_b_units(data))
}

/// True if the access unit contains at least one VCL slice and every VCL slice is intra
/// (IDR, I, or SI) — i.e. it is a random-access point even if not a NAL-type-5 IDR.
pub fn units_are_intra_only(units: &[Vec<u8>]) -> bool {
    let mut saw_vcl = false;

    for unit in units {
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
                0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x41, 0x9A,
                0x22
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
    fn amf_prepends_parameter_sets_on_forced_non_idr_intra_access_unit() {
        // AMF's forced periodic intra can be an all-I-slice that is NOT an IDR (NAL type 1,
        // slice_type I). It is still a random-access point, so it must carry SPS/PPS even though
        // packet_is_key is false and there is no NAL type 5.
        let extradata = [
            1, 66, 0, 30, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x02, 0x68,
            0xCE,
        ];
        // 0x41 = NAL type 1; 0xB8 decodes to an I slice (see access_unit_is_intra_only tests).
        let payload = [0, 0, 0, 1, 0x41, 0xB8];

        let mut state = H264BitstreamState::default();
        state.refresh_parameter_sets_from_extradata(Some(extradata.to_vec()));
        state.packet_count = 90; // mid-stream, not the startup-inject path

        let normalized = normalize_h264_payload(&payload, false, &mut state, "h264_amf");
        assert_eq!(
            normalized,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x41, 0xB8
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
