use flatbuffers::FlatBufferBuilder;

// VTable offsets for FramePacket fields (4 + 2 * field_index).
const VT_SEQ: flatbuffers::VOffsetT = 4;
const VT_TIMESTAMP_US: flatbuffers::VOffsetT = 6;
const VT_DATA: flatbuffers::VOffsetT = 8;
const VT_WIDTH: flatbuffers::VOffsetT = 10;
const VT_HEIGHT: flatbuffers::VOffsetT = 12;
const VT_IS_KEYFRAME: flatbuffers::VOffsetT = 14;

/// Serialize a FramePacket into a FlatBuffer byte vector.
pub fn serialize_frame_packet(
    seq: u32,
    timestamp_us: u64,
    data: &[u8],
    width: u32,
    height: u32,
    is_keyframe: bool,
) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(data.len() + 128);

    // Vectors must be created before starting the table.
    let data_offset = builder.create_vector(data);

    let start = builder.start_table();
    builder.push_slot::<u32>(VT_SEQ, seq, 0);
    builder.push_slot::<u64>(VT_TIMESTAMP_US, timestamp_us, 0);
    builder.push_slot_always(VT_DATA, data_offset);
    builder.push_slot::<u32>(VT_WIDTH, width, 0);
    builder.push_slot::<u32>(VT_HEIGHT, height, 0);
    builder.push_slot::<bool>(VT_IS_KEYFRAME, is_keyframe, false);
    let root = builder.end_table(start);

    builder.finish(root, None);
    builder.finished_data().to_vec()
}

/// A decoded v1 FramePacket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFramePacket {
    pub seq: u32,
    pub timestamp_us: u64,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub is_keyframe: bool,
}

/// Hand-rolled reader matched to `serialize_frame_packet`'s layout — the same
/// shape the iPad's `FramePacket.swift` parses. Used by the fake receiver in
/// end-to-end tests; every read is bounds-checked and returns `None` on any
/// malformed input.
pub fn parse_frame_packet(buf: &[u8]) -> Option<ParsedFramePacket> {
    fn read_u16(buf: &[u8], at: usize) -> Option<u16> {
        Some(u16::from_le_bytes([*buf.get(at)?, *buf.get(at + 1)?]))
    }
    fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_le_bytes([
            *buf.get(at)?,
            *buf.get(at + 1)?,
            *buf.get(at + 2)?,
            *buf.get(at + 3)?,
        ]))
    }
    fn read_u64(buf: &[u8], at: usize) -> Option<u64> {
        Some(u64::from(read_u32(buf, at)?) | (u64::from(read_u32(buf, at + 4)?) << 32))
    }

    let table = read_u32(buf, 0)? as usize;
    // The vtable sits `soffset` BEFORE the table position (signed).
    let soffset = read_u32(buf, table)? as i32;
    let vtable = (table as i64 - soffset as i64).try_into().ok()?;
    let vtable: usize = vtable;
    let vtable_size = read_u16(buf, vtable)? as usize;

    // Field position: vtable[4 + 2*slot_index] is a u16 offset from table start; 0 = absent.
    let field_pos = |vt_offset: flatbuffers::VOffsetT| -> Option<usize> {
        let slot = vt_offset as usize;
        if slot + 2 > vtable_size {
            return None;
        }
        match read_u16(buf, vtable + slot)? {
            0 => None,
            rel => Some(table + rel as usize),
        }
    };

    let seq = match field_pos(VT_SEQ) {
        Some(at) => read_u32(buf, at)?,
        None => 0,
    };
    let timestamp_us = match field_pos(VT_TIMESTAMP_US) {
        Some(at) => read_u64(buf, at)?,
        None => 0,
    };
    let width = match field_pos(VT_WIDTH) {
        Some(at) => read_u32(buf, at)?,
        None => 0,
    };
    let height = match field_pos(VT_HEIGHT) {
        Some(at) => read_u32(buf, at)?,
        None => 0,
    };
    let is_keyframe = match field_pos(VT_IS_KEYFRAME) {
        Some(at) => *buf.get(at)? != 0,
        None => false,
    };

    let data_field = field_pos(VT_DATA)?;
    let vec_start = data_field + read_u32(buf, data_field)? as usize;
    let vec_len = read_u32(buf, vec_start)? as usize;
    let payload = buf.get(vec_start + 4..vec_start + 4 + vec_len)?;

    Some(ParsedFramePacket {
        seq,
        timestamp_us,
        data: payload.to_vec(),
        width,
        height,
        is_keyframe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_frame_packet() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let buf = serialize_frame_packet(42, 123_456, &data, 1920, 1080, true);

        let parsed = parse_frame_packet(&buf).expect("serialized packet parses");
        assert_eq!(
            parsed,
            ParsedFramePacket {
                seq: 42,
                timestamp_us: 123_456,
                data,
                width: 1920,
                height: 1080,
                is_keyframe: true,
            }
        );
    }

    #[test]
    fn default_valued_fields_parse_as_defaults() {
        // seq=0/width=0/height=0/is_keyframe=false are omitted from the vtable
        // by push_slot; the reader must fill in defaults, matching the Swift parser.
        let buf = serialize_frame_packet(0, 0, &[1, 2, 3], 0, 0, false);
        let parsed = parse_frame_packet(&buf).expect("parses");
        assert_eq!(parsed.seq, 0);
        assert_eq!(parsed.timestamp_us, 0);
        assert_eq!(parsed.width, 0);
        assert!(!parsed.is_keyframe);
        assert_eq!(parsed.data, vec![1, 2, 3]);
    }

    #[test]
    fn truncated_and_junk_buffers_never_panic() {
        let buf = serialize_frame_packet(7, 9, &[0xAA; 32], 640, 360, true);
        for len in 0..buf.len() {
            let _ = parse_frame_packet(&buf[..len]);
        }
        let junk: Vec<u8> = (0..64u8).collect();
        let _ = parse_frame_packet(&junk);
    }
}
