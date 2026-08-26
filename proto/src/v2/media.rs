//! v2 media datagrams: one fragment of one encoded video access unit.
//!
//! Wire layout — 32 bytes, little-endian, then `payload_len` bytes of raw
//! Annex B H.264/H.265 (a chunk of the access unit, split at arbitrary byte
//! boundaries):
//!
//! ```text
//! [0..8]   common prefix        (packet_type = 0x01, flags bit0 = keyframe)
//! [8..12]  session_id    u32    from HELLO_ACK; nonzero
//! [12..16] stream_epoch  u32    per-pipeline-run id; nonzero, monotonic per session
//! [16..20] frame_seq     u32    per-epoch access-unit counter
//! [20..22] frag_index    u16
//! [22..24] frag_count    u16    >= 1
//! [24..32] capture_ts_us u64    host monotonic µs since process start
//! ```
//!
//! Every fragment repeats the whole header, so reassembly is stateless with
//! respect to arrival order and any fragment describes its frame completely.

use super::{Classified, CommonPrefix, PacketType, WireError, MAX_DGRAM_SIZE, PREFIX_SIZE};

/// Size of the full media header (common prefix included).
pub const MEDIA_HEADER_SIZE: usize = 32;
/// Largest payload chunk that fits a media datagram.
pub const MAX_MEDIA_PAYLOAD: usize = MAX_DGRAM_SIZE - MEDIA_HEADER_SIZE; // 1368
/// Hard cap on fragments per frame: 4 MiB of payload. Anything larger is a
/// protocol violation (and a memory-exhaustion vector on the receiver).
pub const MAX_FRAG_COUNT: u16 = (4 * 1024 * 1024 / MAX_MEDIA_PAYLOAD) as u16; // 3066

/// flags bit0: the access unit this fragment belongs to is a random-access point.
pub const MEDIA_FLAG_KEYFRAME: u8 = 0b0000_0001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub session_id: u32,
    pub stream_epoch: u32,
    pub frame_seq: u32,
    pub frag_index: u16,
    pub frag_count: u16,
    pub is_keyframe: bool,
    pub capture_ts_us: u64,
    /// Length of the payload chunk following the header.
    pub payload_len: u16,
}

impl MediaHeader {
    /// Writes the 32-byte header into `buf[0..32]`.
    ///
    /// # Panics
    /// Panics if `buf` is shorter than [`MEDIA_HEADER_SIZE`] — send buffers are
    /// fixed-size on the host, so this is a programming error, not a wire error.
    pub fn encode_into(self, buf: &mut [u8]) {
        assert!(buf.len() >= MEDIA_HEADER_SIZE);
        CommonPrefix {
            packet_type: PacketType::Media,
            flags: if self.is_keyframe {
                MEDIA_FLAG_KEYFRAME
            } else {
                0
            },
            payload_len: self.payload_len,
        }
        .encode_into(buf);
        buf[8..12].copy_from_slice(&self.session_id.to_le_bytes());
        buf[12..16].copy_from_slice(&self.stream_epoch.to_le_bytes());
        buf[16..20].copy_from_slice(&self.frame_seq.to_le_bytes());
        buf[20..22].copy_from_slice(&self.frag_index.to_le_bytes());
        buf[22..24].copy_from_slice(&self.frag_count.to_le_bytes());
        buf[24..32].copy_from_slice(&self.capture_ts_us.to_le_bytes());
    }

    /// Parses and validates a media datagram, returning the header and the
    /// payload slice. Rejects anything that violates the protocol invariants so
    /// downstream code never has to re-check them.
    pub fn decode(datagram: &[u8]) -> Result<(Self, &[u8]), WireError> {
        let prefix = CommonPrefix::decode(datagram)?;
        if !matches!(prefix.packet_type, PacketType::Media) {
            return Err(WireError::InvalidField("packet_type"));
        }
        if datagram.len() < MEDIA_HEADER_SIZE {
            return Err(WireError::Truncated);
        }
        if usize::from(prefix.payload_len) != datagram.len() - MEDIA_HEADER_SIZE {
            return Err(WireError::LengthMismatch);
        }

        let get_u32 = |at: usize| {
            u32::from_le_bytes([
                datagram[at],
                datagram[at + 1],
                datagram[at + 2],
                datagram[at + 3],
            ])
        };
        let frag_index = u16::from_le_bytes([datagram[20], datagram[21]]);
        let frag_count = u16::from_le_bytes([datagram[22], datagram[23]]);

        if frag_count == 0 || frag_count > MAX_FRAG_COUNT {
            return Err(WireError::InvalidField("frag_count"));
        }
        if frag_index >= frag_count {
            return Err(WireError::InvalidField("frag_index"));
        }
        let session_id = get_u32(8);
        if session_id == 0 {
            return Err(WireError::InvalidField("session_id"));
        }
        let stream_epoch = get_u32(12);
        if stream_epoch == 0 {
            return Err(WireError::InvalidField("stream_epoch"));
        }

        let header = Self {
            session_id,
            stream_epoch,
            frame_seq: get_u32(16),
            frag_index,
            frag_count,
            is_keyframe: prefix.flags & MEDIA_FLAG_KEYFRAME != 0,
            capture_ts_us: u64::from_le_bytes([
                datagram[24],
                datagram[25],
                datagram[26],
                datagram[27],
                datagram[28],
                datagram[29],
                datagram[30],
                datagram[31],
            ]),
            payload_len: prefix.payload_len,
        };
        Ok((header, &datagram[MEDIA_HEADER_SIZE..]))
    }
}

/// Splits one encoded access unit into ready-to-send datagrams, invoking `emit`
/// with each complete datagram (header + chunk) serialized into `scratch`.
///
/// `scratch` must be at least [`MAX_DGRAM_SIZE`] bytes; the same buffer is
/// reused for every fragment, so `emit` must consume (send/copy) it before
/// returning. Returns the fragment count, or a `WireError` if the payload
/// exceeds [`MAX_FRAG_COUNT`] fragments.
pub fn fragment_access_unit(
    header_template: MediaHeader,
    payload: &[u8],
    scratch: &mut [u8],
    mut emit: impl FnMut(&[u8]),
) -> Result<u16, WireError> {
    assert!(scratch.len() >= MAX_DGRAM_SIZE);
    let frag_count = payload.len().div_ceil(MAX_MEDIA_PAYLOAD).max(1);
    if frag_count > usize::from(MAX_FRAG_COUNT) {
        return Err(WireError::InvalidField("frag_count"));
    }
    let frag_count = frag_count as u16;

    for (index, chunk) in payload
        .chunks(MAX_MEDIA_PAYLOAD)
        .chain(std::iter::once(&payload[..0]).filter(|_| payload.is_empty()))
        .enumerate()
    {
        let header = MediaHeader {
            frag_index: index as u16,
            frag_count,
            payload_len: chunk.len() as u16,
            ..header_template
        };
        header.encode_into(scratch);
        scratch[MEDIA_HEADER_SIZE..MEDIA_HEADER_SIZE + chunk.len()].copy_from_slice(chunk);
        emit(&scratch[..MEDIA_HEADER_SIZE + chunk.len()]);
    }
    Ok(frag_count)
}

/// Convenience: true if this datagram would classify as v2 media.
pub fn is_media_datagram(datagram: &[u8]) -> bool {
    matches!(super::classify(datagram), Classified::Media { .. })
}

const _: () = assert!(PREFIX_SIZE == 8);

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> MediaHeader {
        MediaHeader {
            session_id: 0xA1B2_C3D4,
            stream_epoch: 7,
            frame_seq: 12_345,
            frag_index: 2,
            frag_count: 9,
            is_keyframe: true,
            capture_ts_us: 0x0000_0123_4567_89AB,
            payload_len: 3,
        }
    }

    #[test]
    fn media_header_round_trip() {
        let mut dgram = vec![0u8; MEDIA_HEADER_SIZE + 3];
        sample_header().encode_into(&mut dgram);
        dgram[32..].copy_from_slice(&[0xAA, 0xBB, 0xCC]);

        let (decoded, payload) = MediaHeader::decode(&dgram).unwrap();
        assert_eq!(decoded, sample_header());
        assert_eq!(payload, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn media_header_exact_byte_layout() {
        let mut dgram = vec![0u8; MEDIA_HEADER_SIZE + 3];
        sample_header().encode_into(&mut dgram);

        assert_eq!(&dgram[0..2], &[0x45, 0x4D]); // "EM"
        assert_eq!(dgram[2], 2); // version
        assert_eq!(dgram[3], 0x01); // media
        assert_eq!(dgram[4], MEDIA_FLAG_KEYFRAME);
        assert_eq!(dgram[5], 0); // reserved
        assert_eq!(&dgram[6..8], &[3, 0]); // payload_len LE
        assert_eq!(&dgram[8..12], &[0xD4, 0xC3, 0xB2, 0xA1]); // session_id LE
        assert_eq!(&dgram[12..16], &[7, 0, 0, 0]);
        assert_eq!(&dgram[16..20], &[0x39, 0x30, 0, 0]); // 12345 LE
        assert_eq!(&dgram[20..22], &[2, 0]);
        assert_eq!(&dgram[22..24], &[9, 0]);
        assert_eq!(
            &dgram[24..32],
            &[0xAB, 0x89, 0x67, 0x45, 0x23, 0x01, 0x00, 0x00]
        );
    }

    #[test]
    fn media_decode_rejects_invalid_fields() {
        let mut dgram = vec![0u8; MEDIA_HEADER_SIZE + 3];
        sample_header().encode_into(&mut dgram);

        // Truncated below header size.
        assert_eq!(
            MediaHeader::decode(&dgram[..MEDIA_HEADER_SIZE - 1]),
            Err(WireError::Truncated)
        );

        // payload_len disagrees with datagram length.
        assert_eq!(
            MediaHeader::decode(&dgram[..MEDIA_HEADER_SIZE + 2]),
            Err(WireError::LengthMismatch)
        );

        // Zero session id.
        let mut zero_session = dgram.clone();
        zero_session[8..12].copy_from_slice(&[0; 4]);
        assert_eq!(
            MediaHeader::decode(&zero_session),
            Err(WireError::InvalidField("session_id"))
        );

        // Zero epoch.
        let mut zero_epoch = dgram.clone();
        zero_epoch[12..16].copy_from_slice(&[0; 4]);
        assert_eq!(
            MediaHeader::decode(&zero_epoch),
            Err(WireError::InvalidField("stream_epoch"))
        );

        // frag_index >= frag_count.
        let mut bad_index = dgram.clone();
        bad_index[20..22].copy_from_slice(&9u16.to_le_bytes());
        assert_eq!(
            MediaHeader::decode(&bad_index),
            Err(WireError::InvalidField("frag_index"))
        );

        // Zero frag_count.
        let mut zero_count = dgram.clone();
        zero_count[20..22].copy_from_slice(&0u16.to_le_bytes());
        zero_count[22..24].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            MediaHeader::decode(&zero_count),
            Err(WireError::InvalidField("frag_count"))
        );

        // frag_count above the 4 MiB cap.
        let mut huge_count = dgram.clone();
        huge_count[20..22].copy_from_slice(&0u16.to_le_bytes());
        huge_count[22..24].copy_from_slice(&(MAX_FRAG_COUNT + 1).to_le_bytes());
        assert_eq!(
            MediaHeader::decode(&huge_count),
            Err(WireError::InvalidField("frag_count"))
        );
    }

    #[test]
    fn fragments_split_and_carry_full_metadata() {
        let payload: Vec<u8> = (0..u32::try_from(MAX_MEDIA_PAYLOAD * 2 + 100).unwrap())
            .map(|i| (i % 251) as u8)
            .collect();
        let mut scratch = [0u8; MAX_DGRAM_SIZE];
        let mut datagrams: Vec<Vec<u8>> = Vec::new();

        let count = fragment_access_unit(
            MediaHeader {
                frag_index: 0,
                frag_count: 0,
                payload_len: 0,
                ..sample_header()
            },
            &payload,
            &mut scratch,
            |dgram| datagrams.push(dgram.to_vec()),
        )
        .unwrap();

        assert_eq!(count, 3);
        assert_eq!(datagrams.len(), 3);
        assert_eq!(datagrams[0].len(), MAX_DGRAM_SIZE);
        assert_eq!(datagrams[2].len(), MEDIA_HEADER_SIZE + 100);

        let mut reassembled = Vec::new();
        for (i, dgram) in datagrams.iter().enumerate() {
            let (header, chunk) = MediaHeader::decode(dgram).unwrap();
            assert_eq!(header.frag_index as usize, i);
            assert_eq!(header.frag_count, 3);
            assert_eq!(header.frame_seq, 12_345);
            assert!(header.is_keyframe);
            reassembled.extend_from_slice(chunk);
        }
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn empty_payload_still_emits_one_fragment() {
        let mut scratch = [0u8; MAX_DGRAM_SIZE];
        let mut emitted = 0usize;
        let count = fragment_access_unit(
            MediaHeader {
                frag_index: 0,
                frag_count: 0,
                payload_len: 0,
                ..sample_header()
            },
            &[],
            &mut scratch,
            |dgram| {
                emitted += 1;
                let (header, chunk) = MediaHeader::decode(dgram).unwrap();
                assert_eq!(header.frag_count, 1);
                assert!(chunk.is_empty());
            },
        )
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(emitted, 1);
    }
}
