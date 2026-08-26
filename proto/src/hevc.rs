//! Minimal H.265/HEVC bitstream inspection for the HEVC streaming path.
//!
//! Unlike H.264, the host does no parameter-set surgery for HEVC — hardware
//! encoders emit Annex B with in-band VPS/SPS/PPS when configured for
//! repeating headers — so all that's needed is NAL classification for
//! keyframe detection (and the client's format-description assembly).

/// HEVC NAL unit type from the first byte after a start code:
/// `(byte >> 1) & 0x3F`.
pub fn nal_type(unit_first_byte: u8) -> u8 {
    (unit_first_byte >> 1) & 0x3F
}

pub const NAL_BLA_W_LP: u8 = 16;
pub const NAL_IDR_W_RADL: u8 = 19;
pub const NAL_IDR_N_LP: u8 = 20;
pub const NAL_CRA: u8 = 21;
pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;

/// Intra random access point (IRAP): BLA/IDR/CRA — any of these restarts
/// decode cleanly.
pub fn is_irap(nal_type: u8) -> bool {
    (NAL_BLA_W_LP..=NAL_CRA).contains(&nal_type)
}

/// Scan an Annex B stream for any IRAP slice (keyframe detection).
pub fn contains_keyframe(data: &[u8]) -> bool {
    scan_nal_first_bytes(data).any(|first| is_irap(nal_type(first)))
}

/// Scan for VPS+SPS+PPS presence (a self-contained decoder-init AU).
pub fn contains_parameter_sets(data: &[u8]) -> bool {
    let (mut vps, mut sps, mut pps) = (false, false, false);
    for first in scan_nal_first_bytes(data) {
        match nal_type(first) {
            NAL_VPS => vps = true,
            NAL_SPS => sps = true,
            NAL_PPS => pps = true,
            _ => {}
        }
    }
    vps && sps && pps
}

/// Iterate the first byte of every Annex B NAL unit (3- or 4-byte start codes).
fn scan_nal_first_bytes(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut index = 0usize;
    std::iter::from_fn(move || {
        while index + 3 <= data.len() {
            if data[index] == 0 && data[index + 1] == 0 {
                if data[index + 2] == 1 {
                    let first = *data.get(index + 3)?;
                    index += 4;
                    return Some(first);
                }
                if index + 4 <= data.len() && data[index + 2] == 0 && data[index + 3] == 1 {
                    let first = *data.get(index + 4)?;
                    index += 5;
                    return Some(first);
                }
            }
            index += 1;
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an Annex B stream from HEVC NAL types (2-byte headers).
    fn stream(types: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &t in types {
            out.extend_from_slice(&[0, 0, 0, 1, t << 1, 0x01, 0xAA]);
        }
        out
    }

    #[test]
    fn detects_irap_types() {
        assert!(contains_keyframe(&stream(&[
            NAL_VPS,
            NAL_SPS,
            NAL_PPS,
            NAL_IDR_W_RADL
        ])));
        assert!(contains_keyframe(&stream(&[NAL_CRA])));
        assert!(!contains_keyframe(&stream(&[1]))); // trailing picture
    }

    #[test]
    fn detects_parameter_sets() {
        assert!(contains_parameter_sets(&stream(&[
            NAL_VPS,
            NAL_SPS,
            NAL_PPS,
            NAL_IDR_N_LP
        ])));
        assert!(!contains_parameter_sets(&stream(&[
            NAL_SPS,
            NAL_PPS,
            NAL_IDR_N_LP
        ])));
    }

    #[test]
    fn three_byte_start_codes_work() {
        let mut data = vec![0, 0, 1, NAL_IDR_W_RADL << 1, 0x01];
        data.extend_from_slice(&[0, 0, 1, 1 << 1, 0x01]);
        assert!(contains_keyframe(&data));
    }
}
