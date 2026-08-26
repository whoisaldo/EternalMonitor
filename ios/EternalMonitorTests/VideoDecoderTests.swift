import XCTest
import CoreVideo
import os
@testable import EternalMonitor

/// Runs the real VideoToolbox path. In the simulator this exercises the
/// software decoder — which is exactly the fallback the Enable- (not Require-)
/// hardware specification exists to provide, and what the automated E2E
/// depends on.
final class VideoDecoderTests: XCTestCase {
    private func fixtureData() throws -> Data {
        let url = try XCTUnwrap(
            Bundle(for: VideoDecoderTests.self).url(
                forResource: "single_idr_64x64",
                withExtension: "h264",
                subdirectory: "Fixtures"
            ),
            "single_idr_64x64.h264 fixture missing from the test bundle"
        )
        return try Data(contentsOf: url)
    }

    private func packet(seq: UInt32, data: Data, keyframe: Bool) -> FramePacket {
        FramePacket(
            seq: seq,
            timestampUs: UInt64(seq) * 16_667,
            data: data,
            width: 64,
            height: 64,
            isKeyframe: keyframe
        )
    }

    func testDecodesSingleIDRFixtureToNV12() throws {
        let data = try fixtureData()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }

        let decoded = expectation(description: "one frame decoded")
        var format: OSType = 0
        var width = 0
        var height = 0
        decoder.onFrameDecoded = { pixelBuffer, _ in
            format = CVPixelBufferGetPixelFormatType(pixelBuffer)
            width = CVPixelBufferGetWidth(pixelBuffer)
            height = CVPixelBufferGetHeight(pixelBuffer)
            decoded.fulfill()
        }

        decoder.decode(packet: packet(seq: 1, data: data, keyframe: true))
        wait(for: [decoded], timeout: 10)

        XCTAssertEqual(width, 64)
        XCTAssertEqual(height, 64)
        XCTAssertEqual(
            format, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            "decoder must output NV12 for the Metal YCbCr path"
        )
    }

    func testRepeatedDecodeDoesNotRebuildSessionPerKeyframe() throws {
        // The old per-IDR session recreation produced a "VideoToolbox session
        // ready" event for every keyframe. Now only the first format
        // description may create a session; identical SPS/PPS must not.
        let data = try fixtureData()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }

        let sessionEvents = OSAllocatedUnfairLock(initialState: 0)
        decoder.onEvent = { message in
            if message.hasPrefix("VideoToolbox session ready") {
                sessionEvents.withLock { $0 += 1 }
            }
        }
        let decodedAll = expectation(description: "frames decoded")
        decodedAll.expectedFulfillmentCount = 5
        decoder.onFrameDecoded = { _, _ in decodedAll.fulfill() }

        for seq in 1...5 {
            decoder.decode(packet: packet(seq: UInt32(seq), data: data, keyframe: true))
        }
        wait(for: [decodedAll], timeout: 10)

        XCTAssertEqual(
            sessionEvents.withLock { $0 }, 1,
            "identical keyframes must reuse one VTDecompressionSession"
        )
    }

    func testShutdownDuringDecodeStressDoesNotCrash() throws {
        let data = try fixtureData()
        for _ in 0..<10 {
            let decoder = VideoDecoder()
            for seq in 1...3 {
                decoder.decode(packet: packet(seq: UInt32(seq), data: data, keyframe: true))
            }
            let done = expectation(description: "shutdown completed")
            decoder.shutdown { done.fulfill() }
            wait(for: [done], timeout: 5)
        }
    }
}
