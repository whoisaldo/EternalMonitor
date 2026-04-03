use flatbuffers::FlatBufferBuilder;

const VT_KIND: flatbuffers::VOffsetT = 4;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Connect = 0,
    Disconnect = 1,
    Ping = 2,
}

/// Serialize a ControlMsg into a FlatBuffer byte vector.
pub fn serialize_control_msg(kind: ControlKind) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(32);

    let start = builder.start_table();
    builder.push_slot::<u8>(VT_KIND, kind as u8, 0);
    let root = builder.end_table(start);

    builder.finish(root, None);
    builder.finished_data().to_vec()
}
