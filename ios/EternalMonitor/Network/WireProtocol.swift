import Foundation

/// EternalMonitor wire protocol v2 — the Swift mirror of the Rust
/// `eternal-wire` crate's `v2` module. Byte layouts are pinned by
/// `proto/testdata/v2_vectors.txt`, which both implementations' test suites
/// consume; any change here must keep those vectors green on both sides.
///
/// Every v2 datagram starts with an 8-byte common prefix (little-endian):
/// magic "EM" u16 | version u8 = 2 | packetType u8 | flags u8 | reserved u8 |
/// payloadLen u16 (== datagramLength - fixedHeaderLength, enforced strictly).
enum Wire {
    static let magic: UInt16 = 0x4D45  // the bytes "EM" (0x45 0x4D)
    static let version: UInt8 = 2
    static let prefixSize = 8
    static let maxDatagramSize = 1400
    static let legacyHelloMagic = Data("ETERNALHELLO".utf8)

    enum PacketType: UInt8 {
        case media = 0x01
        case mediaFec = 0x02
        case hello2 = 0x10
        case helloAck = 0x11
        case heartbeat = 0x12
        case bye = 0x13
        case keyframeRequest = 0x14
        case receiverReport = 0x15
        case ping = 0x16
        case pong = 0x17
        case streamConfig = 0x18
        case inputEvent = 0x20
        case error = 0x7F
    }

    enum Classification: Equatable {
        case media(flags: UInt8)
        case control(PacketType)
        case legacyHello
        case unknown
    }

    /// Routes an inbound datagram without copying.
    static func classify(_ datagram: Data) -> Classification {
        if datagram.count >= legacyHelloMagic.count,
           datagram.prefix(legacyHelloMagic.count) == legacyHelloMagic {
            return .legacyHello
        }
        guard datagram.count >= prefixSize else { return .unknown }
        return datagram.withUnsafeBytes { raw -> Classification in
            guard UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 0, as: UInt16.self)) == magic,
                  raw[2] == version,
                  let type = PacketType(rawValue: raw[3])
            else { return .unknown }
            switch type {
            case .media, .mediaFec:
                return .media(flags: raw[4])
            default:
                return .control(type)
            }
        }
    }
}

// MARK: - Media

/// One fragment of one encoded access unit. 32-byte header; payload is a raw
/// Annex B chunk. Every fragment repeats the full frame metadata.
struct MediaHeader: Equatable {
    static let size = 32
    static let maxPayload = Wire.maxDatagramSize - size  // 1368
    /// 4 MiB frame cap — larger fragment counts are protocol violations.
    static let maxFragCount: UInt16 = UInt16((4 * 1024 * 1024) / maxPayload)  // 3066
    static let keyframeFlag: UInt8 = 0b0000_0001

    var sessionId: UInt32
    var streamEpoch: UInt32
    var frameSeq: UInt32
    var fragIndex: UInt16
    var fragCount: UInt16
    var isKeyframe: Bool
    var captureTimestampUs: UInt64
    var payloadLen: UInt16

    /// Parses and validates a media datagram. Returns the header and the
    /// payload range *within the passed Data* (no copy). Enforces every
    /// protocol invariant so downstream code never re-checks them.
    static func decode(_ datagram: Data) -> (header: MediaHeader, payloadRange: Range<Data.Index>)? {
        guard datagram.count >= size else { return nil }
        let header: MediaHeader? = datagram.withUnsafeBytes { raw in
            guard UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 0, as: UInt16.self)) == Wire.magic,
                  raw[2] == Wire.version,
                  raw[3] == Wire.PacketType.media.rawValue
            else { return nil }
            let payloadLen = UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 6, as: UInt16.self))
            guard Int(payloadLen) == datagram.count - size else { return nil }
            let sessionId = UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 8, as: UInt32.self))
            let streamEpoch = UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 12, as: UInt32.self))
            let fragIndex = UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 20, as: UInt16.self))
            let fragCount = UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 22, as: UInt16.self))
            guard sessionId != 0, streamEpoch != 0,
                  fragCount >= 1, fragCount <= maxFragCount, fragIndex < fragCount
            else { return nil }
            return MediaHeader(
                sessionId: sessionId,
                streamEpoch: streamEpoch,
                frameSeq: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 16, as: UInt32.self)),
                fragIndex: fragIndex,
                fragCount: fragCount,
                isKeyframe: raw[4] & keyframeFlag != 0,
                captureTimestampUs: UInt64(littleEndian: raw.loadUnaligned(fromByteOffset: 24, as: UInt64.self)),
                payloadLen: payloadLen
            )
        }
        guard let header else { return nil }
        let start = datagram.index(datagram.startIndex, offsetBy: size)
        return (header, start..<datagram.endIndex)
    }

    /// Serializes a full media datagram (header + payload). Test/tool use.
    func encode(payload: Data) -> Data {
        var out = Data(capacity: Self.size + payload.count)
        out.appendLE(Wire.magic)
        out.append(Wire.version)
        out.append(Wire.PacketType.media.rawValue)
        out.append(isKeyframe ? Self.keyframeFlag : 0)
        out.append(0)
        out.appendLE(UInt16(payload.count))
        out.appendLE(sessionId)
        out.appendLE(streamEpoch)
        out.appendLE(frameSeq)
        out.appendLE(fragIndex)
        out.appendLE(fragCount)
        out.appendLE(captureTimestampUs)
        out.append(payload)
        return out
    }
}

// MARK: - Control messages

struct ControlHeader: Equatable {
    static let size = 16
    var packetType: Wire.PacketType
    var sessionId: UInt32
    var msgSeq: UInt32
}

/// Current stream parameters (16-byte block).
struct StreamConfig: Equatable {
    static let size = 16
    static let codecH264: UInt8 = 0
    static let codecHEVC: UInt8 = 1
    static let flagSoftwareEncoder: UInt8 = 1 << 0

    var streamEpoch: UInt32 = 0
    var width: UInt16 = 0
    var height: UInt16 = 0
    var fps: UInt16 = 0
    var codec: UInt8 = 0
    var flags: UInt8 = 0
    var bitrateBps: UInt32 = 0

    fileprivate func encode(into out: inout Data) {
        out.appendLE(streamEpoch)
        out.appendLE(width)
        out.appendLE(height)
        out.appendLE(fps)
        out.append(codec)
        out.append(flags)
        out.appendLE(bitrateBps)
    }

    fileprivate static func decode(_ r: inout WireReader) -> StreamConfig? {
        guard let streamEpoch: UInt32 = r.readU32(),
              let width: UInt16 = r.readU16(),
              let height: UInt16 = r.readU16(),
              let fps: UInt16 = r.readU16(),
              let codec = r.readU8(),
              let flags = r.readU8(),
              let bitrateBps: UInt32 = r.readU32()
        else { return nil }
        return StreamConfig(
            streamEpoch: streamEpoch, width: width, height: height, fps: fps,
            codec: codec, flags: flags, bitrateBps: bitrateBps
        )
    }
}

struct Hello2: Equatable {
    static let capDecodeH264: UInt16 = 1 << 0
    static let capDecodeHEVC: UInt16 = 1 << 1
    static let capDecodeHEVC10Bit: UInt16 = 1 << 2
    static let featureWantsInput: UInt16 = 1 << 0
    static let featureWantsAudio: UInt16 = 1 << 1
    static let maxNameLength = 64

    var protoMin: UInt8 = 2
    var protoMax: UInt8 = 2
    var clientNonce: UInt32
    var listenPort: UInt16
    var decoderCaps: UInt16
    var featureCaps: UInt16
    var screenPxW: UInt16
    var screenPxH: UInt16
    var screenPtW: UInt16
    var screenPtH: UInt16
    var refreshHz: UInt8
    var deviceName: String
}

enum HelloStatus: UInt8 {
    case ok = 0
    case busy = 1
    case versionUnsupported = 2
    case error = 3
}

struct HelloAck: Equatable {
    var status: HelloStatus
    var acceptedVersion: UInt8
    var clientNonce: UInt32
    var sessionId: UInt32
    var heartbeatIntervalMs: UInt16
    var reportIntervalMs: UInt16
    var livenessTimeoutMs: UInt16
    var streamConfig: StreamConfig
    var hostName: String
}

struct Heartbeat: Equatable {
    var hostTimeUs: UInt64
    var streamConfig: StreamConfig
}

enum ByeReason: UInt8 {
    case userDisconnect = 0
    case appBackground = 1
    case hostShuttingDown = 2
    case error = 3
    case superseded = 4
}

enum KeyframeReason: UInt8 {
    case gapLoss = 0
    case decodeError = 1
    case startup = 2
    case resume = 3
}

struct KeyframeRequest: Equatable {
    var streamEpoch: UInt32
    var lastCompleteSeq: UInt32
    var reason: KeyframeReason
}

struct ReceiverReport: Equatable {
    var streamEpoch: UInt32 = 0
    var highestSeq: UInt32 = 0
    var framesComplete: UInt32 = 0
    var framesDropped: UInt32 = 0
    var fragsReceived: UInt32 = 0
    var fragsLost: UInt32 = 0
    var jitterUs: UInt32 = 0
    var decodeFpsX10: UInt16 = 0
    var assemblerDepth: UInt8 = 0
    var decodeDepth: UInt8 = 0
    var e2eLatencyMsX10: UInt16 = 0
    var rttMsX10: UInt16 = 0
}

struct WirePing: Equatable {
    var t1Us: UInt64
}

struct WirePong: Equatable {
    var t1Us: UInt64
    var t2Us: UInt64
    var t3Us: UInt64
}

struct WireInputEvent: Equatable {
    var inputVer: UInt8 = 1
    var kind: UInt8
    var phase: UInt8
    var buttons: UInt8
    var eventId: UInt32
    var xNorm: UInt16
    var yNorm: UInt16
    var pressureX1000: UInt16 = 0
    var scrollDx: Int16 = 0
    var scrollDy: Int16 = 0
    var keycode: UInt16 = 0
    var modifiers: UInt8 = 0
    var clientTimeUs: UInt64
}

enum ControlMessage: Equatable {
    case hello2(Hello2)
    case helloAck(HelloAck)
    case heartbeat(Heartbeat)
    case bye(ByeReason)
    case keyframeRequest(KeyframeRequest)
    case receiverReport(ReceiverReport)
    case ping(WirePing)
    case pong(WirePong)
    case streamConfig(StreamConfig)
    case inputEvent(WireInputEvent)

    var packetType: Wire.PacketType {
        switch self {
        case .hello2: return .hello2
        case .helloAck: return .helloAck
        case .heartbeat: return .heartbeat
        case .bye: return .bye
        case .keyframeRequest: return .keyframeRequest
        case .receiverReport: return .receiverReport
        case .ping: return .ping
        case .pong: return .pong
        case .streamConfig: return .streamConfig
        case .inputEvent: return .inputEvent
        }
    }
}

extension Wire {
    /// Serializes one control datagram (16-byte header + message body).
    static func encodeControl(sessionId: UInt32, msgSeq: UInt32, message: ControlMessage) -> Data {
        var body = Data(capacity: 64)
        switch message {
        case .hello2(let h):
            let name = Data(h.deviceName.utf8).prefixOnCharBoundary(h.deviceName, max: Hello2.maxNameLength)
            body.append(h.protoMin)
            body.append(h.protoMax)
            body.appendLE(h.clientNonce)
            body.appendLE(h.listenPort)
            body.appendLE(h.decoderCaps)
            body.appendLE(h.featureCaps)
            body.appendLE(h.screenPxW)
            body.appendLE(h.screenPxH)
            body.appendLE(h.screenPtW)
            body.appendLE(h.screenPtH)
            body.append(h.refreshHz)
            body.append(UInt8(name.count))
            body.append(name)
        case .helloAck(let a):
            let name = Data(a.hostName.utf8).prefixOnCharBoundary(a.hostName, max: Hello2.maxNameLength)
            body.append(a.status.rawValue)
            body.append(a.acceptedVersion)
            body.appendLE(a.clientNonce)
            body.appendLE(a.sessionId)
            body.appendLE(a.heartbeatIntervalMs)
            body.appendLE(a.reportIntervalMs)
            body.appendLE(a.livenessTimeoutMs)
            a.streamConfig.encode(into: &body)
            body.append(UInt8(name.count))
            body.append(name)
        case .heartbeat(let hb):
            body.appendLE(hb.hostTimeUs)
            hb.streamConfig.encode(into: &body)
        case .bye(let reason):
            body.append(reason.rawValue)
        case .keyframeRequest(let k):
            body.appendLE(k.streamEpoch)
            body.appendLE(k.lastCompleteSeq)
            body.append(k.reason.rawValue)
        case .receiverReport(let r):
            body.appendLE(r.streamEpoch)
            body.appendLE(r.highestSeq)
            body.appendLE(r.framesComplete)
            body.appendLE(r.framesDropped)
            body.appendLE(r.fragsReceived)
            body.appendLE(r.fragsLost)
            body.appendLE(r.jitterUs)
            body.appendLE(r.decodeFpsX10)
            body.append(r.assemblerDepth)
            body.append(r.decodeDepth)
            body.appendLE(r.e2eLatencyMsX10)
            body.appendLE(r.rttMsX10)
        case .ping(let p):
            body.appendLE(p.t1Us)
        case .pong(let p):
            body.appendLE(p.t1Us)
            body.appendLE(p.t2Us)
            body.appendLE(p.t3Us)
        case .streamConfig(let cfg):
            cfg.encode(into: &body)
        case .inputEvent(let e):
            body.append(e.inputVer)
            body.append(e.kind)
            body.append(e.phase)
            body.append(e.buttons)
            body.appendLE(e.eventId)
            body.appendLE(e.xNorm)
            body.appendLE(e.yNorm)
            body.appendLE(e.pressureX1000)
            body.appendLE(UInt16(bitPattern: e.scrollDx))
            body.appendLE(UInt16(bitPattern: e.scrollDy))
            body.appendLE(e.keycode)
            body.append(e.modifiers)
            body.append(0)  // reserved
            body.appendLE(e.clientTimeUs)
        }

        var out = Data(capacity: ControlHeader.size + body.count)
        out.appendLE(magic)
        out.append(version)
        out.append(message.packetType.rawValue)
        out.append(0)  // flags
        out.append(0)  // reserved
        out.appendLE(UInt16(body.count))
        out.appendLE(sessionId)
        out.appendLE(msgSeq)
        out.append(body)
        return out
    }

    /// Parses one control datagram. Enforces the strict length invariant and
    /// every field invariant; ignores trailing body bytes (append-only
    /// evolution). Returns nil on any violation — parsing never traps.
    static func parseControl(_ datagram: Data) -> (header: ControlHeader, message: ControlMessage)? {
        guard datagram.count >= ControlHeader.size else { return nil }
        let bytes = [UInt8](datagram)
        guard UInt16(bytes[0]) | (UInt16(bytes[1]) << 8) == magic,
              bytes[2] == version,
              let type = PacketType(rawValue: bytes[3])
        else { return nil }
        let payloadLen = UInt16(bytes[6]) | (UInt16(bytes[7]) << 8)
        guard Int(payloadLen) == bytes.count - ControlHeader.size else { return nil }

        let header = ControlHeader(
            packetType: type,
            sessionId: UInt32(littleEndianBytes: bytes, at: 8),
            msgSeq: UInt32(littleEndianBytes: bytes, at: 12)
        )
        var r = WireReader(bytes: bytes, at: ControlHeader.size)

        let message: ControlMessage?
        switch type {
        case .hello2:
            message = parseHello2(&r)
        case .helloAck:
            message = parseHelloAck(&r)
        case .heartbeat:
            guard let hostTimeUs = r.readU64(),
                  let cfg = StreamConfig.decode(&r) else { return nil }
            message = .heartbeat(Heartbeat(hostTimeUs: hostTimeUs, streamConfig: cfg))
        case .bye:
            guard let raw = r.readU8(), let reason = ByeReason(rawValue: raw) else { return nil }
            message = .bye(reason)
        case .keyframeRequest:
            guard let epoch: UInt32 = r.readU32(),
                  let last: UInt32 = r.readU32(),
                  let raw = r.readU8(),
                  let reason = KeyframeReason(rawValue: raw) else { return nil }
            message = .keyframeRequest(
                KeyframeRequest(streamEpoch: epoch, lastCompleteSeq: last, reason: reason))
        case .receiverReport:
            message = parseReceiverReport(&r)
        case .ping:
            guard let t1 = r.readU64() else { return nil }
            message = .ping(WirePing(t1Us: t1))
        case .pong:
            guard let t1 = r.readU64(), let t2 = r.readU64(), let t3 = r.readU64() else { return nil }
            message = .pong(WirePong(t1Us: t1, t2Us: t2, t3Us: t3))
        case .streamConfig:
            guard let cfg = StreamConfig.decode(&r) else { return nil }
            message = .streamConfig(cfg)
        case .inputEvent:
            message = parseInputEvent(&r)
        case .media, .mediaFec, .error:
            return nil
        }
        guard let message else { return nil }
        return (header, message)
    }

    private static func parseHello2(_ r: inout WireReader) -> ControlMessage? {
        guard let protoMin = r.readU8(), let protoMax = r.readU8(), protoMin <= protoMax,
              let clientNonce: UInt32 = r.readU32(),
              let listenPort: UInt16 = r.readU16(),
              let decoderCaps: UInt16 = r.readU16(),
              let featureCaps: UInt16 = r.readU16(),
              let screenPxW: UInt16 = r.readU16(),
              let screenPxH: UInt16 = r.readU16(),
              let screenPtW: UInt16 = r.readU16(),
              let screenPtH: UInt16 = r.readU16(),
              let refreshHz = r.readU8(),
              let deviceName = r.readName()
        else { return nil }
        return .hello2(Hello2(
            protoMin: protoMin, protoMax: protoMax, clientNonce: clientNonce,
            listenPort: listenPort, decoderCaps: decoderCaps, featureCaps: featureCaps,
            screenPxW: screenPxW, screenPxH: screenPxH,
            screenPtW: screenPtW, screenPtH: screenPtH,
            refreshHz: refreshHz, deviceName: deviceName))
    }

    private static func parseHelloAck(_ r: inout WireReader) -> ControlMessage? {
        guard let statusRaw = r.readU8(), let status = HelloStatus(rawValue: statusRaw),
              let acceptedVersion = r.readU8(),
              let clientNonce: UInt32 = r.readU32(),
              let sessionId: UInt32 = r.readU32(),
              let heartbeatIntervalMs: UInt16 = r.readU16(),
              let reportIntervalMs: UInt16 = r.readU16(),
              let livenessTimeoutMs: UInt16 = r.readU16(),
              let streamConfig = StreamConfig.decode(&r),
              let hostName = r.readName()
        else { return nil }
        if status == .ok && sessionId == 0 { return nil }
        return .helloAck(HelloAck(
            status: status, acceptedVersion: acceptedVersion, clientNonce: clientNonce,
            sessionId: sessionId, heartbeatIntervalMs: heartbeatIntervalMs,
            reportIntervalMs: reportIntervalMs, livenessTimeoutMs: livenessTimeoutMs,
            streamConfig: streamConfig, hostName: hostName))
    }

    private static func parseReceiverReport(_ r: inout WireReader) -> ControlMessage? {
        guard let streamEpoch: UInt32 = r.readU32(),
              let highestSeq: UInt32 = r.readU32(),
              let framesComplete: UInt32 = r.readU32(),
              let framesDropped: UInt32 = r.readU32(),
              let fragsReceived: UInt32 = r.readU32(),
              let fragsLost: UInt32 = r.readU32(),
              let jitterUs: UInt32 = r.readU32(),
              let decodeFpsX10: UInt16 = r.readU16(),
              let assemblerDepth = r.readU8(),
              let decodeDepth = r.readU8(),
              let e2eLatencyMsX10: UInt16 = r.readU16(),
              let rttMsX10: UInt16 = r.readU16()
        else { return nil }
        return .receiverReport(ReceiverReport(
            streamEpoch: streamEpoch, highestSeq: highestSeq,
            framesComplete: framesComplete, framesDropped: framesDropped,
            fragsReceived: fragsReceived, fragsLost: fragsLost, jitterUs: jitterUs,
            decodeFpsX10: decodeFpsX10, assemblerDepth: assemblerDepth,
            decodeDepth: decodeDepth, e2eLatencyMsX10: e2eLatencyMsX10, rttMsX10: rttMsX10))
    }

    private static func parseInputEvent(_ r: inout WireReader) -> ControlMessage? {
        guard let inputVer = r.readU8(),
              let kind = r.readU8(),
              let phase = r.readU8(),
              let buttons = r.readU8(),
              let eventId: UInt32 = r.readU32(),
              let xNorm: UInt16 = r.readU16(),
              let yNorm: UInt16 = r.readU16(),
              let pressureX1000: UInt16 = r.readU16(),
              let scrollDxRaw: UInt16 = r.readU16(),
              let scrollDyRaw: UInt16 = r.readU16(),
              let keycode: UInt16 = r.readU16(),
              let modifiers = r.readU8(),
              let _ = r.readU8(),  // reserved
              let clientTimeUs = r.readU64()
        else { return nil }
        return .inputEvent(WireInputEvent(
            inputVer: inputVer, kind: kind, phase: phase, buttons: buttons,
            eventId: eventId, xNorm: xNorm, yNorm: yNorm, pressureX1000: pressureX1000,
            scrollDx: Int16(bitPattern: scrollDxRaw), scrollDy: Int16(bitPattern: scrollDyRaw),
            keycode: keycode, modifiers: modifiers, clientTimeUs: clientTimeUs))
    }
}

// MARK: - Bounds-checked reader

/// Every read either returns the value or nil — indexing can never trap.
private struct WireReader {
    let bytes: [UInt8]
    var at: Int

    mutating func take(_ len: Int) -> ArraySlice<UInt8>? {
        guard at + len <= bytes.count else { return nil }
        defer { at += len }
        return bytes[at..<at + len]
    }

    mutating func readU8() -> UInt8? {
        take(1)?.first
    }

    mutating func readU16() -> UInt16? {
        guard let s = take(2) else { return nil }
        let b = Array(s)
        return UInt16(b[0]) | (UInt16(b[1]) << 8)
    }

    mutating func readU32() -> UInt32? {
        guard let s = take(4) else { return nil }
        let b = Array(s)
        return UInt32(b[0]) | (UInt32(b[1]) << 8) | (UInt32(b[2]) << 16) | (UInt32(b[3]) << 24)
    }

    mutating func readU64() -> UInt64? {
        guard let low = readU32(), let high = readU32() else { return nil }
        return UInt64(low) | (UInt64(high) << 32)
    }

    /// Length-prefixed UTF-8 string, max 64 bytes.
    mutating func readName() -> String? {
        guard let len = readU8(), Int(len) <= Hello2.maxNameLength,
              let slice = take(Int(len))
        else { return nil }
        return String(bytes: slice, encoding: .utf8)
    }
}

// MARK: - Little-endian append helpers

private extension Data {
    mutating func appendLE(_ value: UInt16) {
        append(UInt8(value & 0xFF))
        append(UInt8(value >> 8))
    }

    mutating func appendLE(_ value: UInt32) {
        appendLE(UInt16(value & 0xFFFF))
        appendLE(UInt16(value >> 16))
    }

    mutating func appendLE(_ value: UInt64) {
        appendLE(UInt32(value & 0xFFFF_FFFF))
        appendLE(UInt32(value >> 32))
    }

    /// UTF-8 prefix of `source` that fits `max` bytes without splitting a
    /// character — mirrors the Rust `truncated_name`.
    func prefixOnCharBoundary(_ source: String, max: Int) -> Data {
        if count <= max { return self }
        var end = source.utf8.index(source.utf8.startIndex, offsetBy: max)
        while end != source.utf8.startIndex, !source.isValidUTF8Boundary(end) {
            end = source.utf8.index(before: end)
        }
        return Data(source.utf8[source.utf8.startIndex..<end])
    }
}

private extension String {
    func isValidUTF8Boundary(_ index: String.UTF8View.Index) -> Bool {
        // A UTF-8 boundary is any byte that is not a continuation byte.
        guard index != utf8.endIndex else { return true }
        return utf8[index] & 0b1100_0000 != 0b1000_0000
    }
}

private extension UInt32 {
    init(littleEndianBytes b: [UInt8], at: Int) {
        self = UInt32(b[at]) | (UInt32(b[at + 1]) << 8) | (UInt32(b[at + 2]) << 16)
            | (UInt32(b[at + 3]) << 24)
    }
}
