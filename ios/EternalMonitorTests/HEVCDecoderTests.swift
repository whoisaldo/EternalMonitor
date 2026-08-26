import XCTest
import CoreVideo
import os
@testable import EternalMonitor

/// The HEVC decode path through real VideoToolbox (software in the simulator),
/// plus the codec sniffing that lets the host switch H.264 ↔ HEVC mid-session
/// with a live encoder reopen.
final class HEVCDecoderTests: XCTestCase {
    private func fixture(_ name: String, _ ext: String) throws -> Data {
        let url = try XCTUnwrap(
            Bundle(for: HEVCDecoderTests.self).url(
                forResource: name,
                withExtension: ext,
                subdirectory: "Fixtures"
            ),
            "\(name).\(ext) fixture missing from the test bundle"
        )
        return try Data(contentsOf: url)
    }

    private func packet(seq: UInt32, data: Data) -> FramePacket {
        FramePacket(
            seq: seq,
            timestampUs: UInt64(seq) * 16_667,
            data: data,
            width: 64,
            height: 64,
            isKeyframe: true
        )
    }

    func testDecodesSingleIRAPFixtureToNV12() throws {
        let data = try fixture("single_irap_64x64", "h265")
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }

        let events = OSAllocatedUnfairLock(initialState: [String]())
        decoder.onEvent = { message in events.withLock { $0.append(message) } }

        let decoded = expectation(description: "one HEVC frame decoded")
        var format: OSType = 0
        var width = 0
        var height = 0
        decoder.onFrameDecoded = { pixelBuffer, _ in
            format = CVPixelBufferGetPixelFormatType(pixelBuffer)
            width = CVPixelBufferGetWidth(pixelBuffer)
            height = CVPixelBufferGetHeight(pixelBuffer)
            decoded.fulfill()
        }

        decoder.decode(packet: packet(seq: 1, data: data))
        guard XCTWaiter.wait(for: [decoded], timeout: 10) == .completed else {
            XCTFail("no HEVC frame decoded; decoder events: \(events.withLock { $0 })")
            return
        }

        XCTAssertEqual(width, 64)
        XCTAssertEqual(height, 64)
        XCTAssertEqual(
            format, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            "HEVC must land on the same NV12 Metal path as H.264"
        )
    }

    func testMidSessionCodecSwitchDecodesBothWays() throws {
        // H.264 keyframe, then HEVC, then H.264 again — one decoder instance.
        // This is exactly what the wire carries around a host codec reopen.
        let h264 = try fixture("single_idr_64x64", "h264")
        let hevc = try fixture("single_irap_64x64", "h265")
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }

        let decodedAll = expectation(description: "all three frames decoded")
        decodedAll.expectedFulfillmentCount = 3
        decoder.onFrameDecoded = { _, _ in decodedAll.fulfill() }

        decoder.decode(packet: packet(seq: 1, data: h264))
        decoder.decode(packet: packet(seq: 2, data: hevc))
        decoder.decode(packet: packet(seq: 3, data: h264))
        wait(for: [decodedAll], timeout: 15)
    }

    func testSniffDistinguishesTheCodecs() throws {
        // H.264 SPS (0x67) and HEVC VPS (0x40) first bytes.
        XCTAssertEqual(VideoDecoder.sniffCodec([Data([0x67, 0x42]), Data([0x68])]), .h264)
        XCTAssertEqual(VideoDecoder.sniffCodec([Data([0x40, 0x01]), Data([0x42, 0x01])]), .hevc)
        XCTAssertNil(
            VideoDecoder.sniffCodec([Data([0x41, 0x9A])]),
            "a bare H.264 P-slice (0x41) must not read as an HEVC VPS…"
        )
        XCTAssertNil(VideoDecoder.sniffCodec([]))
    }
}
