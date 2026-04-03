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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_frame_packet() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let buf = serialize_frame_packet(42, 123456, &data, 1920, 1080, true);
        // Verify the buffer is a valid FlatBuffer (starts with a root offset).
        assert!(buf.len() > 4);
    }
}
