import Foundation
@preconcurrency import CoreVideo
import QuartzCore
import Combine
import os
import UIKit

enum ConnectionState: Equatable {
    case disconnected
    case connecting
    case connected
}

enum DiagnosticLevel: String {
    case info = "INFO"
    case warning = "WARN"
    case error = "ERROR"
}

struct DiagnosticEntry: Identifiable {
    let id = UUID()
    let timestamp = Date()
    let level: DiagnosticLevel
    let category: String
    let message: String
}

private struct ConnectionDebugState {
    var host = ""
    var port: UInt16 = 0
    var listenerReadyPort: UInt16?
    var helloAttempts = 0
    var datagramsReceived = 0
    var datagramBytesReceived = 0
    var assembledFrames = 0
    var assembledFrameBytes = 0
    var decodePackets = 0
    var decodedFrames = 0
    var lastDecodedTimestampUs: UInt64 = 0

    var timeoutSummary: String {
        if listenerReadyPort == nil {
            return "UDP listener never became ready."
        }
        if helloAttempts == 0 {
            return "Listener came up, but HELLO registration never sent."
        }
        if datagramsReceived == 0 {
            return "HELLO sent \(helloAttempts)x to \(host):\(port), but no UDP datagrams came back."
        }
        if assembledFrames == 0 {
            return "Received \(datagramsReceived) UDP datagrams (\(datagramBytesReceived) bytes), but no complete frame was reassembled."
        }
        if decodePackets == 0 {
            return "Reassembled \(assembledFrames) frame payloads, but none could be parsed as FramePacket."
        }
        if decodedFrames == 0 {
            return "Parsed \(decodePackets) packets and assembled \(assembledFrames) payloads, but VideoToolbox produced no frames."
        }
        return "Received \(decodedFrames) decoded frames, but the connection did not transition cleanly."
    }
}

@MainActor
final class ConnectionManager: ObservableObject {
    @Published var state: ConnectionState = .disconnected
    @Published var fps: Double = 0
    @Published var lagMs: Double = 0
    @Published var transportMode: String = "WiFi"
    @Published var connectionError: String?
    @Published private(set) var diagnostics: [DiagnosticEntry] = []
    // connection quality tracker exposed to the HUD.
    let quality = ConnectionQualityTracker()

    private var udpReceiver: UDPReceiver?
    private var frameAssembler: FrameAssembler?
    private var videoDecoder: VideoDecoder?
    private var timeoutTask: Task<Void, Never>?
    private var statsFlushTask: Task<Void, Never>?
    /// Hot-path counters written on the UDP queue and drained at 4 Hz — the
    /// old per-datagram MainActor Task (~2 per datagram, ~2,800 hops/s at
    /// 1080p60) throttled the whole receive path.
    private let streamCounters = StreamCounters()
    private var debugState = ConnectionDebugState()
    // Whether we've already granted the one-time timeout extension that fires when datagrams
    // start arriving (so a slow/jittery network gets a fresh window to finish reassembly+decode).
    private var didExtendTimeout = false

    let frameSlot = FrameSlot()
    private var fpsCounter = FPSCounter()

    static let connectionTimeoutSeconds: UInt64 = 10

    // MARK: - Frame slot (thread-safe single-slot)

    var latestFrame: CVPixelBuffer? {
        frameSlot.take()
    }

    // MARK: - Connect / Disconnect

    func connect(host: String, port: UInt16) {
        guard state == .disconnected else { return }
        let normalizedHost = host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedHost.isEmpty else { return }

        state = .connecting
        connectionError = nil
        diagnostics.removeAll()
        didExtendTimeout = false
        debugState = ConnectionDebugState(host: normalizedHost, port: port)
        record(.info, "connection", "Starting connection to \(normalizedHost):\(port)")

        let decoder = VideoDecoder()
        let assembler = FrameAssembler()
        let receiver = UDPReceiver(port: port)

        assembler.onDiagnostic = { [weak self] message in
            Task { @MainActor in
                self?.record(.warning, "assembly", message)
            }
        }

        decoder.onFrameDecoded = { [weak self] pixelBuffer, timestampUs in
            guard let self else { return }
            // Store frame in lock-protected slot (no Sendable boundary crossed)
            self.frameSlot.set(pixelBuffer)
            // Capture only the scalars we need — avoids sending CVPixelBuffer across boundary
            let ts = timestampUs
            let frameWidth = CVPixelBufferGetWidth(pixelBuffer)
            let frameHeight = CVPixelBufferGetHeight(pixelBuffer)
            Task { @MainActor in
                self.quality.recordFrameDecoded()
                self.debugState.decodedFrames += 1
                self.debugState.lastDecodedTimestampUs = ts
                if self.debugState.decodedFrames == 1 {
                    self.record(.info, "decode", "First frame decoded successfully")
                    self.timeoutTask?.cancel()
                    self.state = .connected
                    self.transportMode = "WiFi"
                    // Streaming is a passive activity — never let the iPad dim
                    // and lock mid-session because nobody touched the screen.
                    UIApplication.shared.isIdleTimerDisabled = true
                    E2E.firstFrame(width: frameWidth, height: frameHeight)
                    RecentConnectionStore.shared.add(host: normalizedHost, port: port, isUSB: false)
                    // remember the last successfully-connected target so we
                    // can pre-fill the IP field on next launch.
                    UserDefaults.standard.set(normalizedHost, forKey: "lastHost")
                    UserDefaults.standard.set(Int(port), forKey: "lastPort")
                }
                self.fpsCounter.tick()
                self.fps = self.fpsCounter.currentFPS
                if E2E.enabled && self.debugState.decodedFrames % 60 == 0 {
                    E2E.stats(
                        decoded: self.debugState.decodedFrames,
                        width: frameWidth,
                        height: frameHeight,
                        fps: self.fps
                    )
                }
                let nowUs = UInt64(ProcessInfo.processInfo.systemUptime * 1_000_000)
                if ts > 0 {
                    self.lagMs = Double(nowUs &- ts) / 1000.0
                }
            }
        }

        decoder.onNeedsKeyframe = { [weak self] in
            Task { @MainActor in
                // Protocol v2 turns this into a keyframe request to the host;
                // until then the stream recovers on the next natural IDR.
                self?.record(.warning, "decode", "Decoder needs a keyframe to resume")
            }
        }

        decoder.onEvent = { [weak self] message in
            Task { @MainActor in
                self?.record(.info, "decode", message)
            }
        }

        assembler.onFrameAssembled = { [weak self, weak decoder] data in
            guard let self else { return }
            self.streamCounters.recordAssembled(bytes: data.count)

            guard let decoder else { return }
            guard let packet = FramePacket.deserialize(from: data) else {
                Task { @MainActor in
                    self.record(.warning, "proto", "Failed to parse reassembled payload (\(data.count) bytes) as FramePacket")
                }
                return
            }
            self.streamCounters.recordParsed()
            decoder.decode(packet: packet)
        }

        receiver.assembler = assembler
        receiver.onListenerReady = { [weak self] actualPort in
            Task { @MainActor in
                self?.debugState.listenerReadyPort = actualPort
                self?.record(.info, "udp", "Listener ready on port \(actualPort)")
            }
        }
        receiver.onHelloAttempt = { [weak self] attempt, total, host, port in
            Task { @MainActor in
                self?.debugState.helloAttempts = attempt
                self?.record(.info, "udp", "HELLO attempt \(attempt)/\(total) to \(host):\(port)")
            }
        }
        receiver.onHelloFailure = { [weak self] message in
            Task { @MainActor in
                self?.record(.warning, "udp", "HELLO send failed: \(message)")
            }
        }
        receiver.onDatagramReceived = { [streamCounters] byteCount in
            // UDP-queue side: just count. The 4 Hz flush loop below moves the
            // totals onto the MainActor.
            streamCounters.recordDatagram(bytes: byteCount)
        }
        receiver.onDatagramIgnored = { [weak self] message in
            Task { @MainActor in
                self?.record(.warning, "udp", message)
            }
        }
        receiver.onConnectionEstablished = { [weak self] in
            Task { @MainActor in
                self?.record(.info, "udp", "Accepted UDP stream from selected host")
            }
        }
        receiver.onError = { [weak self] message in
            Task { @MainActor in
                guard let self, self.state == .connecting else { return }
                self.record(.error, "udp", message)
                self.connectionError = message
                self.disconnect()
            }
        }

        self.videoDecoder = decoder
        self.frameAssembler = assembler
        self.udpReceiver = receiver

        guard receiver.start(host: normalizedHost) else {
            connectionError = "Failed to start the receiver."
            record(.error, "connection", "Receiver failed to start")
            disconnect()
            return
        }

        startStatsFlushLoop()

        // Timeout after N seconds (re-armed once when datagrams start arriving).
        armConnectTimeout(seconds: Self.connectionTimeoutSeconds)
    }

    /// Drains the hot-path counters onto the MainActor at 4 Hz — cheap enough
    /// to be invisible, fresh enough for diagnostics and the connect flow.
    private func startStatsFlushLoop() {
        statsFlushTask?.cancel()
        statsFlushTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 250_000_000)
                guard let self, !Task.isCancelled else { return }
                let snapshot = self.streamCounters.drain()
                guard snapshot.datagrams > 0 || snapshot.assembled > 0 else { continue }

                let firstDatagrams = self.debugState.datagramsReceived == 0 && snapshot.datagrams > 0
                let firstAssembled = self.debugState.assembledFrames == 0 && snapshot.assembled > 0
                self.debugState.datagramsReceived += snapshot.datagrams
                self.debugState.datagramBytesReceived += snapshot.datagramBytes
                self.debugState.assembledFrames += snapshot.assembled
                self.debugState.assembledFrameBytes += snapshot.assembledBytes
                self.debugState.decodePackets += snapshot.parsed

                if firstDatagrams {
                    self.record(
                        .info, "udp",
                        "UDP data flowing (\(snapshot.datagrams) datagrams, \(snapshot.datagramBytes) bytes in first batch)"
                    )
                    // Data is flowing — grant a fresh full window (once) so a slow/jittery
                    // network gets time to finish reassembly + decode instead of being cut
                    // off mid-handshake.
                    if self.state == .connecting && !self.didExtendTimeout {
                        self.didExtendTimeout = true
                        self.armConnectTimeout(seconds: Self.connectionTimeoutSeconds)
                    }
                }
                if firstAssembled {
                    self.record(.info, "assembly", "First frame payload assembled (\(snapshot.assembledBytes) bytes)")
                }
            }
        }
    }

    /// (Re)arm the connect-timeout watchdog. Cancels any existing timer and starts a fresh one;
    /// if we're still `.connecting` when it fires, surface a diagnostic and disconnect.
    private func armConnectTimeout(seconds: UInt64) {
        timeoutTask?.cancel()
        timeoutTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: seconds * 1_000_000_000)
            if !Task.isCancelled && self.state == .connecting {
                let summary = self.debugState.timeoutSummary
                self.record(.error, "timeout", summary)
                self.connectionError = "Timed out waiting for frames. \(summary)"
                self.disconnect()
            }
        }
    }

    func cancel() {
        timeoutTask?.cancel()
        disconnect()
    }

    func disconnect() {
        timeoutTask?.cancel()
        timeoutTask = nil
        statsFlushTask?.cancel()
        statsFlushTask = nil
        _ = streamCounters.drain()
        didExtendTimeout = false

        // Teardown order matters: stop the socket first (no new input), then shut
        // the decoder down on its own queue (provably after any in-flight decode).
        udpReceiver?.stop()
        videoDecoder?.shutdown()
        frameAssembler?.reset()

        udpReceiver = nil
        frameAssembler = nil
        videoDecoder = nil

        state = .disconnected
        fps = 0
        lagMs = 0
        quality.reset()
        UIApplication.shared.isIdleTimerDisabled = false
    }

    private func record(_ level: DiagnosticLevel, _ category: String, _ message: String) {
        diagnostics.append(DiagnosticEntry(level: level, category: category, message: message))
        if diagnostics.count > 80 {
            diagnostics.removeFirst(diagnostics.count - 80)
        }
        Logger(subsystem: "com.eternal.monitor", category: category)
            .log(level: level.osLogType, "\(message, privacy: .public)")
    }
}

private extension DiagnosticLevel {
    var osLogType: OSLogType {
        switch self {
        case .info:
            return .info
        case .warning:
            return .default
        case .error:
            return .error
        }
    }
}

// MARK: - Hot-path stream counters

/// Written from the UDP queue on every datagram/frame; drained by the
/// MainActor flush loop. The unfair lock costs nanoseconds where a per-event
/// `Task { @MainActor }` cost a scheduler hop.
final class StreamCounters: @unchecked Sendable {
    struct Snapshot {
        var datagrams = 0
        var datagramBytes = 0
        var assembled = 0
        var assembledBytes = 0
        var parsed = 0
    }

    private let lock = OSAllocatedUnfairLock(initialState: Snapshot())

    func recordDatagram(bytes: Int) {
        lock.withLock {
            $0.datagrams += 1
            $0.datagramBytes += bytes
        }
    }

    func recordAssembled(bytes: Int) {
        lock.withLock {
            $0.assembled += 1
            $0.assembledBytes += bytes
        }
    }

    func recordParsed() {
        lock.withLock { $0.parsed += 1 }
    }

    func drain() -> Snapshot {
        lock.withLock { current in
            let snapshot = current
            current = Snapshot()
            return snapshot
        }
    }
}

// MARK: - Thread-safe single-slot pixel buffer
// CVPixelBuffer is not Sendable in strict concurrency. We wrap it in an
// unchecked-Sendable box so the lock-protected slot can be shared across
// queues safely (the lock serialises all access).

final class PixelBufferBox: @unchecked Sendable {
    let buffer: CVPixelBuffer

    init(_ buffer: CVPixelBuffer) {
        self.buffer = buffer
    }
}

final class FrameSlot: @unchecked Sendable {
    private let lock = os.OSAllocatedUnfairLock<PixelBufferBox?>(initialState: nil)
    /// Fired (on the storing thread — the VideoToolbox callback) every time a
    /// frame lands, so the renderer can schedule an on-demand redraw. Assigned
    /// once by the Metal view before streaming starts.
    var onFrameStored: (() -> Void)?

    func set(_ buffer: CVPixelBuffer) {
        let box = PixelBufferBox(buffer)
        lock.withLock { $0 = box }
        onFrameStored?()
    }

    func take() -> CVPixelBuffer? {
        lock.withLock { current in
            let wrapped = current
            current = nil
            return wrapped?.buffer
        }
    }
}

// MARK: - FPS counter

struct FPSCounter {
    private var timestamps: [CFTimeInterval] = []
    var currentFPS: Double = 0

    mutating func tick() {
        let now = CACurrentMediaTime()
        timestamps.append(now)
        timestamps.removeAll { now - $0 > 1.0 }
        currentFPS = Double(timestamps.count)
    }
}

// MARK: - Settings

final class AppSettings: ObservableObject {
    @Published var preferUSB: Bool {
        didSet { UserDefaults.standard.set(preferUSB, forKey: "preferUSB") }
    }
    @Published var showHUD: Bool {
        didSet { UserDefaults.standard.set(showHUD, forKey: "showHUD") }
    }
    @Published var autoReconnect: Bool {
        didSet { UserDefaults.standard.set(autoReconnect, forKey: "autoReconnect") }
    }
    @Published var targetFPS: Int {
        didSet { UserDefaults.standard.set(targetFPS, forKey: "targetFPS") }
    }
    @Published var promotionEnabled: Bool {
        didSet { UserDefaults.standard.set(promotionEnabled, forKey: "promotionEnabled") }
    }
    // last successfully-connected host+port for pre-fill on relaunch.
    @Published var lastHost: String {
        didSet { UserDefaults.standard.set(lastHost, forKey: "lastHost") }
    }
    @Published var lastPort: UInt16 {
        didSet { UserDefaults.standard.set(Int(lastPort), forKey: "lastPort") }
    }

    init() {
        let defaults = UserDefaults.standard
        self.preferUSB = defaults.object(forKey: "preferUSB") as? Bool ?? true
        self.showHUD = defaults.object(forKey: "showHUD") as? Bool ?? true
        self.autoReconnect = defaults.object(forKey: "autoReconnect") as? Bool ?? true
        self.targetFPS = defaults.object(forKey: "targetFPS") as? Int ?? 60
        self.promotionEnabled = defaults.object(forKey: "promotionEnabled") as? Bool ?? true
        self.lastHost = defaults.object(forKey: "lastHost") as? String ?? ""
        let port = defaults.object(forKey: "lastPort") as? Int ?? 0
        self.lastPort = UInt16(clamping: port)
    }
}

// MARK: - Recent connections persistence

struct RecentConnection: Codable, Identifiable {
    var id: String { "\(host):\(port)" }
    let host: String
    let port: UInt16
    let lastUsed: Date
    let isUSB: Bool
}

final class RecentConnectionStore: ObservableObject {
    static let shared = RecentConnectionStore()

    @Published var connections: [RecentConnection] = []

    private let key = "recentConnections"

    private init() {
        load()
    }

    func add(host: String, port: UInt16, isUSB: Bool) {
        connections.removeAll { $0.host == host && $0.port == port }
        connections.insert(
            RecentConnection(host: host, port: port, lastUsed: Date(), isUSB: isUSB),
            at: 0
        )
        if connections.count > 10 { connections = Array(connections.prefix(10)) }
        save()
    }

    private func load() {
        guard let data = UserDefaults.standard.data(forKey: key),
              let decoded = try? JSONDecoder().decode([RecentConnection].self, from: data) else { return }
        connections = decoded
    }

    private func save() {
        guard let data = try? JSONEncoder().encode(connections) else { return }
        UserDefaults.standard.set(data, forKey: key)
    }
}
