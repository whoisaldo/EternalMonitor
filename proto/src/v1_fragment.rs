pub const MAX_DGRAM_SIZE: usize = 1400;
pub const HEADER_SIZE: usize = 16;
pub const MAX_PAYLOAD_SIZE: usize = MAX_DGRAM_SIZE - HEADER_SIZE; // 1384

/// Fixed-size header prepended to every UDP datagram.
///
/// Wire format (little-endian):
///   [0..4]  seq: u32
///   [4..6]  fragment_index: u16
///   [6..8]  fragment_count: u16
///   [8..12] payload_len: u32
///   [12..16] stream_epoch: u32  (was reserved; older receivers ignore it)
#[derive(Debug, Clone, Copy)]
pub struct FragmentHeader {
    pub seq: u32,
    pub fragment_index: u16,
    pub fragment_count: u16,
    pub payload_len: u32,
    /// Per-pipeline-run identifier. Changes on every stream restart so the receiver can drop the
    /// old stream's reassembly state instantly instead of inferring a restart from a seq gap.
    /// Occupies the previously-reserved tail bytes, so pre-epoch receivers stay wire-compatible.
    pub stream_epoch: u32,
}

impl FragmentHeader {
    pub fn to_bytes(self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&self.seq.to_le_bytes());
        buf[4..6].copy_from_slice(&self.fragment_index.to_le_bytes());
        buf[6..8].copy_from_slice(&self.fragment_count.to_le_bytes());
        buf[8..12].copy_from_slice(&self.payload_len.to_le_bytes());
        buf[12..16].copy_from_slice(&self.stream_epoch.to_le_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8; HEADER_SIZE]) -> Self {
        Self {
            seq: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            fragment_index: u16::from_le_bytes([buf[4], buf[5]]),
            fragment_count: u16::from_le_bytes([buf[6], buf[7]]),
            payload_len: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            stream_epoch: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let header = FragmentHeader {
            seq: 0xDEAD_BEEF,
            fragment_index: 3,
            fragment_count: 700,
            payload_len: 1388,
            stream_epoch: 0x00C0_FFEE,
        };
        let bytes = header.to_bytes();
        let recovered = FragmentHeader::from_bytes(&bytes);
        assert_eq!(recovered.seq, header.seq);
        assert_eq!(recovered.fragment_index, header.fragment_index);
        assert_eq!(recovered.fragment_count, header.fragment_count);
        assert_eq!(recovered.payload_len, header.payload_len);
        assert_eq!(recovered.stream_epoch, header.stream_epoch);
    }

    #[test]
    fn header_is_little_endian() {
        let header = FragmentHeader {
            seq: 0x0102_0304,
            fragment_index: 0x0506,
            fragment_count: 0x0708,
            payload_len: 0x090A_0B0C,
            stream_epoch: 0x0D0E_0F10,
        };
        let bytes = header.to_bytes();
        assert_eq!(&bytes[0..4], &[0x04, 0x03, 0x02, 0x01]);
        assert_eq!(&bytes[4..6], &[0x06, 0x05]);
        assert_eq!(&bytes[6..8], &[0x08, 0x07]);
        assert_eq!(&bytes[8..12], &[0x0C, 0x0B, 0x0A, 0x09]);
        assert_eq!(&bytes[12..16], &[0x10, 0x0F, 0x0E, 0x0D]);
    }
}
