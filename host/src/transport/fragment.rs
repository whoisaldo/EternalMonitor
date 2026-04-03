pub const MAX_DGRAM_SIZE: usize = 1400;
pub const HEADER_SIZE: usize = 12;
pub const MAX_PAYLOAD_SIZE: usize = MAX_DGRAM_SIZE - HEADER_SIZE; // 1388

/// Fixed-size header prepended to every UDP datagram.
///
/// Wire format (little-endian):
///   [0..4]  seq: u32
///   [4]     fragment_index: u8
///   [5]     fragment_count: u8
///   [6..8]  reserved: [u8; 2]
///   [8..12] payload_len: u32
#[derive(Debug, Clone, Copy)]
pub struct FragmentHeader {
    pub seq: u32,
    pub fragment_index: u8,
    pub fragment_count: u8,
    pub payload_len: u32,
}

impl FragmentHeader {
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&self.seq.to_le_bytes());
        buf[4] = self.fragment_index;
        buf[5] = self.fragment_count;
        // buf[6..8] reserved, already zero
        buf[8..12].copy_from_slice(&self.payload_len.to_le_bytes());
        buf
    }

    #[allow(dead_code)]
    pub fn from_bytes(buf: &[u8; HEADER_SIZE]) -> Self {
        Self {
            seq: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            fragment_index: buf[4],
            fragment_count: buf[5],
            payload_len: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
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
            fragment_count: 7,
            payload_len: 1388,
        };
        let bytes = header.to_bytes();
        let recovered = FragmentHeader::from_bytes(&bytes);
        assert_eq!(recovered.seq, header.seq);
        assert_eq!(recovered.fragment_index, header.fragment_index);
        assert_eq!(recovered.fragment_count, header.fragment_count);
        assert_eq!(recovered.payload_len, header.payload_len);
    }
}
