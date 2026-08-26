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
    /// Live stream health for the HUD, recomputed at the 4 Hz flush.
    @Published private(set) var stats = StreamStats()
    /// Host heartbeats/frames stopped arriving; the frozen frame is stale.
    @Published private(set) var signalLost = false
    /// Nonzero while an automatic reconnect cycle is running.
    @Published private(set) var reconnectAttempt = 0

    private var udpReceiver: UDPReceiver?
    private var frameAssembler: FrameAssembler?
    private var controlChannel: ControlChannel?
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
    /// Lets hot-path closures reach the CURRENT control channel without
    /// retaining self (rebuilt each connect).
    private let controlChannelBox = ControlChannelBox()
    private var fpsCounter = FPSCounter()
    private var lastTarget: (host: String, port: UInt16)?
    private var livenessTimeoutUs: UInt64 = 3_000_000
    private var degradedSinceUs: UInt64?
    private var reconnectTask: Task<Void, Never>?
    private var prevCounters = FrameAssembler.Counters()

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
        lastTarget = (normalizedHost, port)
        signalLost = false
        degradedSinceUs = nil
        prevCounters = FrameAssembler.Counters()
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
                self.signalLost = false
                self.degradedSinceUs = nil
                if ts > 0,
                   let offset = self.controlChannelBox.value?.clockSnapshot.withLock({ $0.offsetUs }) {
                    let nowUs = ControlChannel.clientNowUs()
                    let captureClient = Int64(bitPattern: ts) - offset
                    let latencyUs = Int64(bitPattern: nowUs) - captureClient
                    self.lagMs = latencyUs > 0 ? Double(latencyUs) / 1000.0 : 0
                }
            }
        }

        decoder.onNeedsKeyframe = { [weak controlChannelBox] in
            controlChannelBox?.value?.sendKeyframeRequest(
                streamEpoch: 0, lastCompleteSeq: 0, reason: .decodeError
            )
        }

        decoder.onEvent = { [weak self] message in
            Task { @MainActor in
                self?.record(.info, "decode", message)
            }
        }

        assembler.onFrameAssembled = { [weak self, weak decoder] data, seq, captureTsUs, isKeyframe in
            guard let self else { return }
            self.streamCounters.recordAssembled(bytes: data.count)
            self.streamCounters.recordParsed()
            decoder?.decode(packet: FramePacket(
                seq: seq,
                timestampUs: captureTsUs,
                data: data,
                width: 0,
                height: 0,
                isKeyframe: isKeyframe
            ))
        }

        receiver.assembler = assembler

        let channel = ControlChannel(queue: receiver.controlQueue, send: { [weak receiver] data in
            receiver?.send(data)
        })
        controlChannelBox.value = channel
        channel.reportProvider = { [weak assembler, weak channel] in
            var report = ReceiverReport()
            if let counters = assembler?.counters.withLock({ $0 }) {
                report.framesComplete = UInt32(clamping: counters.framesComplete)
                report.framesDropped = UInt32(clamping: counters.framesDropped)
                report.fragsReceived = UInt32(clamping: counters.fragsReceived)
                report.fragsLost = UInt32(clamping: counters.fragsLost)
            }
            if let snapshot = channel?.clockSnapshot.withLock({ $0 }) {
                if let rtt = snapshot.rttUs {
                    report.rttMsX10 = UInt16(clamping: rtt / 100)
                }
            }
            return report
        }
        channel.onHelloAttempt = { [weak self] attempt, total in
            Task { @MainActor in
                self?.debugState.helloAttempts = attempt
                self?.record(.info, "ctrl", "HELLO2 attempt \(attempt)/\(total) to \(normalizedHost):\(port)")
            }
        }
        channel.onDiagnostic = { [weak self] message in
            Task { @MainActor in
                self?.record(.info, "ctrl", message)
            }
        }
        channel.onSessionEstablished = { [weak self, weak receiver] info in
            receiver?.setAcceptedSessionId(info.sessionId)
            Task { @MainActor in
                guard let self else { return }
                self.livenessTimeoutUs = UInt64(info.livenessTimeoutMs) * 1000
                self.reconnectAttempt = 0
                self.record(
                    .info, "ctrl",
                    "Connected to \(info.hostName): \(info.streamConfig.width)x\(info.streamConfig.height) @ \(info.streamConfig.fps)fps"
                )
            }
        }
        channel.onRejected = { [weak self] status in
            Task { @MainActor in
                guard let self else { return }
                switch status {
                case .busy:
                    self.connectionError = "The host is busy with another device."
                case .versionUnsupported:
                    self.connectionError = "Protocol mismatch — update the Windows host and this app."
                default:
                    self.connectionError = "The host refused the connection."
                }
                self.record(.error, "ctrl", self.connectionError ?? "rejected")
                self.disconnect()
            }
        }
        channel.onHandshakeTimeout = { [weak self, weak receiver] in
            Task { @MainActor in
                guard let self else { return }
                if let receiver, receiver.legacyLookingDatagrams > 0 {
                    self.connectionError =
                        "The host is running v0.1.x — update EternalMonitor on the PC."
                    self.record(.error, "ctrl", "Legacy v1 host detected")
                    self.disconnect()
                } else {
                    self.record(.warning, "ctrl", "HELLO2 handshake got no reply yet")
                }
            }
        }
        channel.onBye = { [weak self] reason in
            Task { @MainActor in
                guard let self else { return }
                self.connectionError = "The host ended the session."
                self.record(.info, "ctrl", "Host BYE (\(reason))")
                self.disconnect()
            }
        }
        channel.onHeartbeat = { _ in
            // Liveness watchdog lands with the reliability phase.
        }

        receiver.onControlDatagram = { [weak channel] data in
            channel?.handleControl(data)
        }
        receiver.onListenerReady = { [weak self, weak channel] actualPort in
            channel?.startHandshake(
                listenPort: actualPort,
                identity: ControlChannel.ClientIdentity(
                    deviceName: UIDevice.current.name,
                    screenPxW: UInt16(clamping: Int(UIScreen.main.nativeBounds.width)),
                    screenPxH: UInt16(clamping: Int(UIScreen.main.nativeBounds.height)),
                    screenPtW: UInt16(clamping: Int(UIScreen.main.bounds.width)),
                    screenPtH: UInt16(clamping: Int(UIScreen.main.bounds.height)),
                    refreshHz: UInt8(clamping: UIScreen.main.maximumFramesPerSecond)
                )
            )
            Task { @MainActor in
                self?.debugState.listenerReadyPort = actualPort
                self?.record(.info, "udp", "Socket ready on ephemeral port \(actualPort)")
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
        self.controlChannel = channel
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
                self.refreshStatsAndWatchdog()
            }
        }
    }

    /// Recomputes the HUD stats from the hot-path counters and runs the
    /// heartbeat liveness watchdog. MainActor, 4 Hz.
    private func refreshStatsAndWatchdog() {
        guard let channel = controlChannel else { return }

        // --- Stats snapshot ---
        var next = StreamStats()
        next.decodeFps = fps
        let clock = channel.clockSnapshot.withLock { $0 }
        if let rtt = clock.rttUs { next.rttMs = Double(rtt) / 1000.0 }
        if clock.offsetUs != nil { next.e2eMs = lagMs }
        if let counters = frameAssembler?.counters.withLock({ $0 }) {
            let deltaReceived = counters.fragsReceived &- prevCounters.fragsReceived
            let deltaLost = counters.fragsLost &- prevCounters.fragsLost
            prevCounters = counters
            let total = deltaReceived + deltaLost
            next.lossPercent = total == 0 ? 0 : Double(deltaLost) * 100.0 / Double(total)
            next.framesDropped = counters.framesDropped
        }
        next.bars = StreamStats.bars(lossPercent: next.lossPercent, rttMs: next.rttMs)
        stats = next

        // --- Liveness watchdog (host heartbeats stopped => stale picture) ---
        guard state == .connected else { return }
        let lastHeartbeat = channel.lastHeartbeatAtUs.withLock { $0 }
        guard lastHeartbeat > 0 else { return }
        let sinceUs = ControlChannel.clientNowUs() &- lastHeartbeat
        if sinceUs > livenessTimeoutUs {
            if !signalLost {
                signalLost = true
                degradedSinceUs = ControlChannel.clientNowUs()
                record(.warning, "ctrl", "Host heartbeats stopped — signal lost")
            } else if let since = degradedSinceUs,
                      ControlChannel.clientNowUs() &- since > 5_000_000 {
                // 5s of degraded: give up on this session and (optionally)
                // start the reconnect cycle.
                beginReconnect(reason: "host stopped responding")
            }
        } else if signalLost {
            signalLost = false
            degradedSinceUs = nil
            record(.info, "ctrl", "Signal recovered")
        }
    }

    private func beginReconnect(reason: String) {
        let target = lastTarget
        let auto = UserDefaults.standard.object(forKey: "autoReconnect") as? Bool ?? true
        record(.warning, "ctrl", "Connection lost (\(reason))")
        disconnect()
        guard auto, let target else {
            connectionError = "Connection lost — \(reason)."
            return
        }
        startReconnectCycle(to: target)
    }

    private func startReconnectCycle(to target: (host: String, port: UInt16)) {
        reconnectTask?.cancel()
        reconnectTask = Task { @MainActor [weak self] in
            for attempt in 1...6 {
                guard let self, !Task.isCancelled else { return }
                self.reconnectAttempt = attempt
                self.connectionError = "Connection lost — reconnecting (attempt \(attempt)/6)…"
                let backoff = min(1 << (attempt - 1), 15)
                try? await Task.sleep(for: .seconds(Double(backoff)))
                guard !Task.isCancelled, self.state == .disconnected else { return }
                self.connect(host: target.host, port: target.port)
                // Give the attempt its connect-timeout window to succeed.
                try? await Task.sleep(for: .seconds(Double(Self.connectionTimeoutSeconds) + 2))
                guard !Task.isCancelled else { return }
                if self.state == .connected {
                    self.reconnectAttempt = 0
                    self.connectionError = nil
                    return
                }
            }
            guard let self, !Task.isCancelled else { return }
            self.reconnectAttempt = 0
            self.connectionError = "Could not reach the host after 6 attempts."
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
        reconnectTask?.cancel()
        reconnectAttempt = 0
        disconnect()
    }

    /// The app is leaving the foreground: say goodbye so the host stops
    /// streaming promptly instead of waiting out its liveness window.
    func handleAppBackgrounded() {
        guard state != .disconnected else { return }
        controlChannel?.sendBye(.appBackground)
        record(.info, "ctrl", "App backgrounded — sent BYE and disconnected")
        disconnect()
    }

    func disconnect() {
        timeoutTask?.cancel()
        timeoutTask = nil
        statsFlushTask?.cancel()
        statsFlushTask = nil
        _ = streamCounters.drain()
        didExtendTimeout = false

        // Say goodbye first (fire-and-forget), then stop the socket, then shut
        // the decoder down on its own queue (provably after any in-flight decode).
        controlChannel?.sendBye(.userDisconnect)
        controlChannel?.stop()
        udpReceiver?.stop()
        videoDecoder?.shutdown()
        frameAssembler?.reset()

        controlChannel = nil
        controlChannelBox.value = nil
        udpReceiver = nil
        frameAssembler = nil
        videoDecoder = nil

        state = .disconnected
        fps = 0
        lagMs = 0
        stats = StreamStats()
        signalLost = false
        degradedSinceUs = nil
        prevCounters = FrameAssembler.Counters()
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

// MARK: - Control channel box

/// Thread-safe holder so non-MainActor closures can reach the live channel.
final class ControlChannelBox: @unchecked Sendable {
    private let lock = OSAllocatedUnfairLock<ControlChannel?>(initialState: nil)
    var value: ControlChannel? {
        get { lock.withLock { $0 } }
        set { lock.withLock { $0 = newValue } }
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
