import XCTest
@testable import EternalMonitor

final class FramePacketTests: XCTestCase {
    /// `eternal_wire::frame::serialize_frame_packet(42, 123_456, [DE AD BE EF], 1920, 1080, true)`
    /// captured byte-for-byte from the Rust serializer — the same bytes a real
    /// host puts on the wire.
    private static let canonical = Data(hex:
        "14000000100024002000140010000c0008000700100000000000000138040000800700001400000040e2010000000000000000002a00000004000000deadbeef"
    )

    func testParsesCanonicalRustSerializedPacket() throws {
        let packet = try XCTUnwrap(FramePacket.deserialize(from: Self.canonical))
        XCTAssertEqual(packet.seq, 42)
        XCTAssertEqual(packet.timestampUs, 123_456)
        XCTAssertEqual(packet.data, Data([0xDE, 0xAD, 0xBE, 0xEF]))
        XCTAssertEqual(packet.width, 1920)
        XCTAssertEqual(packet.height, 1080)
        XCTAssertTrue(packet.isKeyframe)
    }

    func testEveryTruncationIsRejectedWithoutCrashing() {
        for length in 0..<Self.canonical.count {
            XCTAssertNil(
                FramePacket.deserialize(from: Self.canonical.prefix(length)),
                "truncated to \(length) bytes must not parse"
            )
        }
    }

    func testSingleByteMutationsNeverCrash() {
        // Any parse result is acceptable; crashing or reading out of bounds is not.
        for index in 0..<Self.canonical.count {
            for flip in [UInt8(0x01), 0x80, 0xFF] {
                var mutated = Self.canonical
                mutated[mutated.startIndex + index] ^= flip
                _ = FramePacket.deserialize(from: mutated)
            }
        }
    }

    func testRandomBuffersNeverCrash() {
        var state: UInt64 = 0x0BAD_F00D_DEAD_BEEF
        func next() -> UInt64 {
            state ^= state << 13
            state ^= state >> 7
            state ^= state << 17
            return state
        }
        for _ in 0..<5000 {
            let length = Int(next() % 96)
            var bytes = [UInt8]()
            bytes.reserveCapacity(length)
            for _ in 0..<length { bytes.append(UInt8(next() & 0xFF)) }
            _ = FramePacket.deserialize(from: Data(bytes))
        }
    }
}

private extension Data {
    init(hex: String) {
        self.init(capacity: hex.count / 2)
        var iterator = hex.unicodeScalars.makeIterator()
        while let high = iterator.next(), let low = iterator.next() {
            let byte = (UInt8(String(high), radix: 16) ?? 0) << 4
                | (UInt8(String(low), radix: 16) ?? 0)
            append(byte)
        }
    }
}
