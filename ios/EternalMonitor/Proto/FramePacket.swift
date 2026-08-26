import Foundation

/// One reassembled access unit handed to the decoder. In protocol v2 every
/// field arrives in the media fragment header (repeated per datagram), so this
/// is a plain value — the old hand-rolled FlatBuffers parser is gone with the
/// v1 wire format.
struct FramePacket {
    let seq: UInt32
    let timestampUs: UInt64
    /// Raw Annex B H.264 for one access unit.
    let data: Data
    let width: UInt32
    let height: UInt32
    let isKeyframe: Bool
}
