import XCTest
@testable import EternalMonitor

/// Consumes `proto/testdata/v2_vectors.txt` — the exact bytes the Rust
/// `eternal-wire` golden test pins — and proves the Swift codecs agree with
/// them in both directions. The expected field values below are deliberately
/// hardcoded here (not derived from the file) so a wire change must be made
/// consciously on both sides in the same commit.
final class WireProtocolGoldenTests: XCTestCase {
    private static var vectors: [String: Data] = [:]

    override class func setUp() {
        super.setUp()
        guard let url = Bundle(for: WireProtocolGoldenTests.self)
            .url(forResource: "v2_vectors", withExtension: "txt", subdirectory: "testdata"),
            let text = try? String(contentsOf: url, encoding: .utf8)
        else { return }

        for line in text.split(separator: "\n") {
            let line = line.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty, !line.hasPrefix("#") else { continue }
            let parts = line.split(separator: " ", maxSplits: 1)
            guard parts.count == 2 else { continue }
            vectors[String(parts[0])] = Data(hex: String(parts[1]))
        }
    }

    private func vector(_ name: String) throws -> Data {
        let data = try XCTUnwrap(
            Self.vectors[name],
            "golden vector \(name) missing — is proto/testdata bundled and up to date?"
        )
        return data
    }

    func testGoldenFileIsBundledAndComplete() throws {
        XCTAssertEqual(
            Self.vectors.count, 13,
            "golden vector count drifted — update both test suites together"
        )
    }

    // MARK: Media

    func testMediaKeyframeVector() throws {
        let data = try vector("media_keyframe")
        XCTAssertEqual(Wire.classify(data), .media(flags: 1))
        let (header, payloadRange) = try XCTUnwrap(MediaHeader.decode(data))
        XCTAssertEqual(header.sessionId, 0xA1B2_C3D4)
        XCTAssertEqual(header.streamEpoch, 7)
        XCTAssertEqual(header.frameSeq, 12_345)
        XCTAssertEqual(header.fragIndex, 2)
        XCTAssertEqual(header.fragCount, 9)
        XCTAssertTrue(header.isKeyframe)
        XCTAssertEqual(header.captureTimestampUs, 0x0000_0123_4567_89AB)
        XCTAssertEqual(Data(data[payloadRange]), Data([0xDE, 0xAD, 0xBE, 0xEF]))
        XCTAssertEqual(header.encode(payload: Data(data[payloadRange])), data)
    }

    func testMediaDeltaVector() throws {
        let data = try vector("media_delta")
        let (header, payloadRange) = try XCTUnwrap(MediaHeader.decode(data))
        XCTAssertEqual(header.frameSeq, 12_346)
        XCTAssertEqual(header.fragIndex, 0)
        XCTAssertEqual(header.fragCount, 1)
        XCTAssertFalse(header.isKeyframe)
        XCTAssertEqual(header.captureTimestampUs, 999_999)
        XCTAssertEqual(header.encode(payload: Data(data[payloadRange])), data)
    }

    // MARK: Control

    func testHello2Vector() throws {
        let data = try vector("hello2")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(header.sessionId, 0)
        XCTAssertEqual(header.msgSeq, 1)
        guard case .hello2(let hello) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(hello.protoMin, 2)
        XCTAssertEqual(hello.protoMax, 2)
        XCTAssertEqual(hello.clientNonce, 0xDEAD_BEEF)
        XCTAssertEqual(hello.listenPort, 9876)
        XCTAssertEqual(hello.decoderCaps, Hello2.capDecodeH264 | Hello2.capDecodeHEVC)
        XCTAssertEqual(hello.featureCaps, Hello2.featureWantsInput)
        XCTAssertEqual(hello.screenPxW, 2420)
        XCTAssertEqual(hello.screenPxH, 1668)
        XCTAssertEqual(hello.screenPtW, 1210)
        XCTAssertEqual(hello.screenPtH, 834)
        XCTAssertEqual(hello.refreshHz, 120)
        XCTAssertEqual(hello.deviceName, "Ali's iPad Pro")
        XCTAssertEqual(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: message), data)
    }

    func testHelloAckOkVector() throws {
        let data = try vector("hello_ack_ok")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(ack.status, .ok)
        XCTAssertEqual(ack.clientNonce, 0xDEAD_BEEF)
        XCTAssertEqual(ack.sessionId, 0x1234_5678)
        XCTAssertEqual(ack.heartbeatIntervalMs, 1000)
        XCTAssertEqual(ack.reportIntervalMs, 500)
        XCTAssertEqual(ack.livenessTimeoutMs, 3000)
        XCTAssertEqual(ack.streamConfig, Self.sampleConfig)
        XCTAssertEqual(ack.hostName, "ALI-PC")
        XCTAssertEqual(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: message), data)
    }

    func testHelloAckBusyVector() throws {
        let data = try vector("hello_ack_busy")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(header.msgSeq, 2)
        guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(ack.status, .busy)
        XCTAssertEqual(ack.sessionId, 0)
        XCTAssertEqual(ack.streamConfig, StreamConfig())
    }

    func testHeartbeatVector() throws {
        let data = try vector("heartbeat")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(header.sessionId, 0x1234_5678)
        XCTAssertEqual(header.msgSeq, 42)
        guard case .heartbeat(let hb) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(hb.hostTimeUs, 987_654_321)
        XCTAssertEqual(hb.streamConfig, Self.sampleConfig)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: 42, message: message), data)
    }

    func testByeVector() throws {
        let data = try vector("bye_background")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(message, .bye(.appBackground))
    }

    func testKeyframeRequestVector() throws {
        let data = try vector("keyframe_request")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(
            message,
            .keyframeRequest(KeyframeRequest(streamEpoch: 3, lastCompleteSeq: 4111, reason: .gapLoss))
        )
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: 44, message: message), data)
    }

    func testReceiverReportVector() throws {
        let data = try vector("receiver_report")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        var expected = ReceiverReport()
        expected.streamEpoch = 3
        expected.highestSeq = 5000
        expected.framesComplete = 4990
        expected.framesDropped = 10
        expected.fragsReceived = 120_000
        expected.fragsLost = 250
        expected.jitterUs = 2100
        expected.decodeFpsX10 = 599
        expected.assemblerDepth = 1
        expected.decodeDepth = 0
        expected.e2eLatencyMsX10 = 321
        expected.rttMsX10 = 28
        XCTAssertEqual(message, .receiverReport(expected))
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: 45, message: message), data)
    }

    func testPingPongVectors() throws {
        let ping = try vector("ping")
        let (_, pingMessage) = try XCTUnwrap(Wire.parseControl(ping))
        XCTAssertEqual(pingMessage, .ping(WirePing(t1Us: 0x0102_0304_0506_0708)))

        let pong = try vector("pong")
        let (_, pongMessage) = try XCTUnwrap(Wire.parseControl(pong))
        XCTAssertEqual(
            pongMessage,
            .pong(WirePong(
                t1Us: 0x0102_0304_0506_0708,
                t2Us: 0x0102_0304_0506_0710,
                t3Us: 0x0102_0304_0506_0720
            ))
        )
    }

    func testStreamConfigVector() throws {
        let data = try vector("stream_config")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(message, .streamConfig(Self.sampleConfig))
    }

    func testInputEventVector() throws {
        let data = try vector("input_touch_move")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        var expected = WireInputEvent(
            kind: 0, phase: 1, buttons: 1, eventId: 77,
            xNorm: 32_768, yNorm: 16_384, clientTimeUs: 1_000_001
        )
        expected.pressureX1000 = 500
        expected.scrollDx = -3
        expected.scrollDy = 12
        XCTAssertEqual(message, .inputEvent(expected))
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: 48, message: message), data)
    }

    // MARK: Hardening

    func testEveryVectorTruncationIsRejected() throws {
        for (name, data) in Self.vectors {
            let isMedia = name.hasPrefix("media")
            for length in 0..<data.count {
                let slice = data.prefix(length)
                if isMedia {
                    XCTAssertNil(
                        MediaHeader.decode(slice),
                        "\(name) truncated to \(length) bytes must not parse"
                    )
                } else {
                    XCTAssertNil(
                        Wire.parseControl(slice),
                        "\(name) truncated to \(length) bytes must not parse"
                    )
                }
            }
        }
    }

    func testClassification() throws {
        var legacy = Data("ETERNALHELLO".utf8)
        legacy.append(contentsOf: [0x94, 0x26])
        XCTAssertEqual(Wire.classify(legacy), .legacyHello)
        XCTAssertEqual(Wire.classify(Data([0, 1, 2])), .unknown)
        XCTAssertEqual(Wire.classify(Data(repeating: 0xFF, count: 64)), .unknown)
        XCTAssertEqual(Wire.classify(try vector("ping")), .control(.ping))
    }

    func testRandomBytesNeverCrashTheParsers() {
        var state: UInt64 = 0x1234_5678_9ABC_DEFF
        func next() -> UInt64 {
            state ^= state << 13
            state ^= state >> 7
            state ^= state << 17
            return state
        }
        for _ in 0..<5000 {
            let length = Int(next() % 80)
            var bytes = [UInt8]()
            bytes.reserveCapacity(length)
            for _ in 0..<length { bytes.append(UInt8(next() & 0xFF)) }
            let data = Data(bytes)
            _ = Wire.classify(data)
            _ = Wire.parseControl(data)
            _ = MediaHeader.decode(data)
        }
    }

    private static let sampleConfig = StreamConfig(
        streamEpoch: 3, width: 2560, height: 1440, fps: 60,
        codec: StreamConfig.codecH264, flags: 0, bitrateBps: 15_000_000
    )
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
