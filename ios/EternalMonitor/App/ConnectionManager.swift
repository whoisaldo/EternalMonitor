import Foundation
@preconcurrency import CoreVideo
import QuartzCore
import Combine
import os

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

    private var udpReceiver: UDPReceiver?
    private var frameAssembler: FrameAssembler?
    private var videoDecoder: VideoDecoder?
    private var timeoutTask: Task<Void, Never>?
    private var debugState = ConnectionDebugState()

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
        debugState = ConnectionDebugState(host: normalizedHost, port: port)
        record(.info, "connection", "Starting connection to \(normalizedHost):\(port)")

        let decoder = VideoDecoder()
        let assembler = FrameAssembler()
        let receiver = UDPReceiver(port: port)

        decoder.onFrameDecoded = { [weak self] pixelBuffer, timestampUs in
            guard let self else { return }
            // Store frame in lock-protected slot (no Sendable boundary crossed)
            self.frameSlot.set(pixelBuffer)
            // Capture only the scalar we need — avoids sending CVPixelBuffer across boundary
            let ts = timestampUs
            Task { @MainActor in
                self.debugState.decodedFrames += 1
                self.debugState.lastDecodedTimestampUs = ts
                if self.debugState.decodedFrames == 1 {
                    self.record(.info, "decode", "First frame decoded successfully")
                    self.timeoutTask?.cancel()
                    self.state = .connected
                    self.transportMode = "WiFi"
                    RecentConnectionStore.shared.add(host: normalizedHost, port: port, isUSB: false)
                }
                self.fpsCounter.tick()
                self.fps = self.fpsCounter.currentFPS
                let nowUs = UInt64(ProcessInfo.processInfo.systemUptime * 1_000_000)
                if ts > 0 {
                    self.lagMs = Double(nowUs &- ts) / 1000.0
                }
            }
        }

        decoder.onEvent = { [weak self] message in
            Task { @MainActor in
                self?.record(.info, "decode", message)
            }
        }

        assembler.onFrameAssembled = { [weak self, weak decoder] data in
            guard let self else { return }
            Task { @MainActor in
                self.debugState.assembledFrames += 1
                self.debugState.assembledFrameBytes += data.count
                if self.debugState.assembledFrames == 1 || self.debugState.assembledFrames % 30 == 0 {
                    self.record(.info, "assembly", "Assembled frame payload #\(self.debugState.assembledFrames) (\(data.count) bytes)")
                }
            }

            guard let decoder else { return }
            guard let packet = FramePacket.deserialize(from: data) else {
                Task { @MainActor in
                    self.record(.warning, "proto", "Failed to parse reassembled payload (\(data.count) bytes) as FramePacket")
                }
                return
            }
            Task { @MainActor in
                self.debugState.decodePackets += 1
                if self.debugState.decodePackets == 1 || self.debugState.decodePackets % 30 == 0 {
                    self.record(.info, "proto", "Parsed FramePacket seq=\(packet.seq) bytes=\(packet.data.count) keyframe=\(packet.isKeyframe)")
                }
            }
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
        receiver.onDatagramReceived = { [weak self] byteCount in
            Task { @MainActor in
                guard let self else { return }
                self.debugState.datagramsReceived += 1
                self.debugState.datagramBytesReceived += byteCount
                if self.debugState.datagramsReceived == 1 {
                    self.record(.info, "udp", "First UDP datagram received (\(byteCount) bytes)")
                } else if self.debugState.datagramsReceived % 50 == 0 {
                    self.record(.info, "udp", "Received \(self.debugState.datagramsReceived) UDP datagrams so far")
                }
            }
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

        // Timeout after N seconds
        timeoutTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: Self.connectionTimeoutSeconds * 1_000_000_000)
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

        udpReceiver?.stop()
        videoDecoder?.invalidate()
        frameAssembler?.reset()

        udpReceiver = nil
        frameAssembler = nil
        videoDecoder = nil

        state = .disconnected
        fps = 0
        lagMs = 0
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

    func set(_ buffer: CVPixelBuffer) {
        let box = PixelBufferBox(buffer)
        lock.withLock { $0 = box }
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

    init() {
        let defaults = UserDefaults.standard
        self.preferUSB = defaults.object(forKey: "preferUSB") as? Bool ?? true
        self.showHUD = defaults.object(forKey: "showHUD") as? Bool ?? true
        self.autoReconnect = defaults.object(forKey: "autoReconnect") as? Bool ?? true
        self.targetFPS = defaults.object(forKey: "targetFPS") as? Int ?? 60
        self.promotionEnabled = defaults.object(forKey: "promotionEnabled") as? Bool ?? true
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
