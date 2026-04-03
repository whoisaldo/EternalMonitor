import Foundation

/// Manual FlatBuffers deserializer for FramePacket.
/// Matches the Rust `proto/src/frame.rs` VTable offsets exactly.
///
/// Schema:
///   table FramePacket {
///     seq:          uint32;   // VT offset 4
///     timestamp_us: uint64;   // VT offset 6
///     data:         [ubyte];  // VT offset 8
///     width:        uint32;   // VT offset 10
///     height:       uint32;   // VT offset 12
///     is_keyframe:  bool;     // VT offset 14
///   }
struct FramePacket {
    let seq: UInt32
    let timestampUs: UInt64
    let data: Data
    let width: UInt32
    let height: UInt32
    let isKeyframe: Bool

    static func deserialize(from buffer: Data) -> FramePacket? {
        guard buffer.count >= 4 else { return nil }

        return buffer.withUnsafeBytes { raw -> FramePacket? in
            let base = raw.baseAddress!

            // Root offset: first 4 bytes (LE) point to the root table
            let rootOffset = Int(load(base, offset: 0) as UInt32)
            let tableStart = rootOffset
            guard tableStart >= 0, tableStart + 4 <= buffer.count else { return nil }

            // VTable: signed offset from table start (points backwards)
            let vtableSignedOffset = Int(load(base, offset: tableStart) as Int32)
            let vtableStart = tableStart - vtableSignedOffset
            guard vtableStart >= 0, vtableStart + 4 <= buffer.count else { return nil }

            // VTable layout: [vtableSize: u16, objectSize: u16, field0: u16, field1: u16, ...]
            let vtableSize = Int(load(base, offset: vtableStart) as UInt16)
            guard vtableSize >= 4, vtableStart + vtableSize <= buffer.count else { return nil }
            let fieldCount = (vtableSize - 4) / 2  // subtract vtableSize + objectSize header

            // Helper: read field offset from vtable slot
            func fieldOffset(_ vtSlot: Int) -> Int? {
                let slotIndex = (vtSlot - 4) / 2  // VT offsets start at 4, each is 2 bytes
                guard slotIndex < fieldCount else { return nil }
                let off = Int(load(base, offset: vtableStart + 4 + slotIndex * 2) as UInt16)
                guard off != 0 else { return nil }
                let absolute = tableStart + off
                guard absolute >= 0, absolute < buffer.count else { return nil }
                return absolute
            }

            // seq (uint32) — VT 4
            guard let seqOff = fieldOffset(4) else { return nil }
            let seq: UInt32 = load(base, offset: seqOff)

            // timestamp_us (uint64) — VT 6
            let timestampUs: UInt64
            if let off = fieldOffset(6) {
                timestampUs = load(base, offset: off)
            } else {
                timestampUs = 0
            }

            // data ([ubyte] vector) — VT 8
            guard let dataVecOffPos = fieldOffset(8) else { return nil }
            let dataVecRelOffset = Int(load(base, offset: dataVecOffPos) as UInt32)
            let dataVecStart = dataVecOffPos + dataVecRelOffset
            guard dataVecStart >= 0, dataVecStart + 4 <= buffer.count else { return nil }
            let dataLen = Int(load(base, offset: dataVecStart) as UInt32)
            let dataStart = dataVecStart + 4
            guard dataStart + dataLen <= buffer.count else { return nil }
            let data = Data(bytes: base + dataStart, count: dataLen)

            // width (uint32) — VT 10
            let width: UInt32
            if let off = fieldOffset(10) {
                width = load(base, offset: off)
            } else {
                width = 0
            }

            // height (uint32) — VT 12
            let height: UInt32
            if let off = fieldOffset(12) {
                height = load(base, offset: off)
            } else {
                height = 0
            }

            // is_keyframe (bool) — VT 14
            let isKeyframe: Bool
            if let off = fieldOffset(14) {
                guard off < buffer.count else { return nil }
                isKeyframe = (base + off).load(as: UInt8.self) != 0
            } else {
                isKeyframe = false
            }

            return FramePacket(
                seq: seq,
                timestampUs: timestampUs,
                data: data,
                width: width,
                height: height,
                isKeyframe: isKeyframe
            )
        }
    }
}

// MARK: - Little-endian load helper

private func load<T: FixedWidthInteger>(_ base: UnsafeRawPointer, offset: Int) -> T {
    (base + offset).loadUnaligned(as: T.self).littleEndian
}
