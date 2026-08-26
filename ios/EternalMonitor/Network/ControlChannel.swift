import Foundation
import os

/// The client side of the protocol-v2 control plane, sharing the media socket.
/// Owns the HELLO2 handshake (retry until acked), periodic receiver reports
/// (which double as the client's liveness signal), keyframe requests, and BYE.
///
/// Threading: all state lives on `queue`; inbound datagrams are handed over
/// via `handleControl`, callbacks fire on `queue` — the ConnectionManager hops
/// to the MainActor itself.
final class ControlChannel {
    struct SessionInfo {
        let sessionId: UInt32
        let hostName: String
        let heartbeatIntervalMs: UInt16
        let reportIntervalMs: UInt16
        let livenessTimeoutMs: UInt16
        let streamConfig: StreamConfig
    }

    var onSessionEstablished: ((SessionInfo) -> Void)?
    var onRejected: ((HelloStatus) -> Void)?
    /// HELLO2 went unanswered for the whole retry budget.
    var onHandshakeTimeout: (() -> Void)?
    var onHelloAttempt: ((Int, Int) -> Void)?
    var onHeartbeat: ((Heartbeat) -> Void)?
    var onBye: ((ByeReason) -> Void)?
    var onDiagnostic: ((String) -> Void)?
    /// Snapshot provider for periodic receiver reports.
    var reportProvider: (() -> ReceiverReport)?

    private let queue: DispatchQueue
    private let send: (Data) -> Void

    private var sessionId: UInt32 = 0
    private var msgSeq: UInt32 = 0
    private var clientNonce: UInt32 = 0
    private var helloBytes = Data()
    private var helloAttempts = 0
    private var helloTimer: DispatchSourceTimer?
    private var reportTimer: DispatchSourceTimer?
    private var pingTimer: DispatchSourceTimer?
    private var burstPingsRemaining = 0
    private var estimator = ClockEstimator()

    /// Client clock (µs) of the last host heartbeat; 0 = none yet. Read by the
    /// ConnectionManager's watchdog.
    let lastHeartbeatAtUs = OSAllocatedUnfairLock<UInt64>(initialState: 0)
    /// Latest clock-sync results for latency math and the HUD.
    let clockSnapshot = OSAllocatedUnfairLock<ClockSnapshot>(initialState: ClockSnapshot())

    struct ClockSnapshot {
        var offsetUs: Int64?
        var rttUs: UInt64?
    }

    /// Monotonic client clock in microseconds (same domain as CACurrentMediaTime).
    static func clientNowUs() -> UInt64 {
        clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1000
    }

    private static let maxHelloAttempts = 8
    private static let helloRetryMs = 250
    private static let pingIntervalMs = 2000
    private static let pingBurstCount = 5
    private static let pingBurstSpacingMs = 200

    init(queue: DispatchQueue, send: @escaping (Data) -> Void) {
        self.queue = queue
        self.send = send
    }

    var currentSessionId: UInt32 {
        queue.sync { sessionId }
    }

    // MARK: - Handshake

    struct ClientIdentity {
        var deviceName: String
        var screenPxW: UInt16
        var screenPxH: UInt16
        var screenPtW: UInt16
        var screenPtH: UInt16
        var refreshHz: UInt8
        var decoderCaps: UInt16 = Hello2.capDecodeH264
        var featureCaps: UInt16 = 0
    }

    /// Begin the HELLO2 retry loop. `listenPort` is the ephemeral local port
    /// the media stream should target.
    func startHandshake(listenPort: UInt16, identity: ClientIdentity) {
        queue.async { [self] in
            sessionId = 0
            msgSeq = 0
            helloAttempts = 0
            clientNonce = UInt32.random(in: 1...UInt32.max)
            let hello = Hello2(
                clientNonce: clientNonce,
                listenPort: listenPort,
                decoderCaps: identity.decoderCaps,
                featureCaps: identity.featureCaps,
                screenPxW: identity.screenPxW,
                screenPxH: identity.screenPxH,
                screenPtW: identity.screenPtW,
                screenPtH: identity.screenPtH,
                refreshHz: identity.refreshHz,
                deviceName: identity.deviceName
            )
            helloBytes = Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .hello2(hello))

            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(
                deadline: .now(),
                repeating: .milliseconds(Self.helloRetryMs)
            )
            timer.setEventHandler { [weak self] in self?.helloTick() }
            helloTimer?.cancel()
            helloTimer = timer
            timer.resume()
        }
    }

    private func helloTick() {
        guard sessionId == 0 else {
            helloTimer?.cancel()
            helloTimer = nil
            return
        }
        helloAttempts += 1
        if helloAttempts > Self.maxHelloAttempts {
            helloTimer?.cancel()
            helloTimer = nil
            onDiagnostic?("HELLO2 unanswered after \(Self.maxHelloAttempts) attempts")
            onHandshakeTimeout?()
            return
        }
        onHelloAttempt?(helloAttempts, Self.maxHelloAttempts)
        send(helloBytes)
    }

    // MARK: - Inbound

    /// Called on `queue` with a datagram already classified as control.
    func handleControl(_ datagram: Data) {
        guard let (header, message) = Wire.parseControl(datagram) else {
            onDiagnostic?("Dropped malformed control datagram (\(datagram.count) bytes)")
            return
        }

        switch message {
        case .helloAck(let ack):
            handleAck(ack)
        case .heartbeat(let heartbeat):
            guard header.sessionId == sessionId, sessionId != 0 else { return }
            lastHeartbeatAtUs.withLock { $0 = Self.clientNowUs() }
            onHeartbeat?(heartbeat)
        case .bye(let reason):
            guard header.sessionId == sessionId, sessionId != 0 else { return }
            onBye?(reason)
        case .pong(let pong):
            guard header.sessionId == sessionId, sessionId != 0 else { return }
            let t4 = Self.clientNowUs()
            estimator.addExchange(t1: pong.t1Us, t2: pong.t2Us, t3: pong.t3Us, t4: t4)
            clockSnapshot.withLock {
                $0 = ClockSnapshot(offsetUs: estimator.offsetUs, rttUs: estimator.rttUs)
            }
        case .streamConfig:
            break
        default:
            break
        }
    }

    private func handleAck(_ ack: HelloAck) {
        guard ack.clientNonce == clientNonce else {
            onDiagnostic?("Ignored HELLO_ACK for a stale nonce")
            return
        }
        helloTimer?.cancel()
        helloTimer = nil

        guard ack.status == .ok else {
            onRejected?(ack.status)
            return
        }
        guard sessionId == 0 else { return } // duplicate ack retransmit

        sessionId = ack.sessionId
        onDiagnostic?("Session \(ack.sessionId) established with \(ack.hostName)")
        lastHeartbeatAtUs.withLock { $0 = Self.clientNowUs() }
        startReportTimer(intervalMs: max(ack.reportIntervalMs, 100))
        startPinging()
        onSessionEstablished?(SessionInfo(
            sessionId: ack.sessionId,
            hostName: ack.hostName,
            heartbeatIntervalMs: ack.heartbeatIntervalMs,
            reportIntervalMs: ack.reportIntervalMs,
            livenessTimeoutMs: ack.livenessTimeoutMs,
            streamConfig: ack.streamConfig
        ))
    }

    // MARK: - Outbound

    private func startReportTimer(intervalMs: UInt16) {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(
            deadline: .now() + .milliseconds(Int(intervalMs)),
            repeating: .milliseconds(Int(intervalMs))
        )
        timer.setEventHandler { [weak self] in
            guard let self, self.sessionId != 0 else { return }
            let report = self.reportProvider?() ?? ReceiverReport()
            self.sendMessage(.receiverReport(report))
        }
        reportTimer?.cancel()
        reportTimer = timer
        timer.resume()
    }

    func sendKeyframeRequest(streamEpoch: UInt32, lastCompleteSeq: UInt32, reason: KeyframeReason) {
        queue.async { [self] in
            guard sessionId != 0 else { return }
            onDiagnostic?("Requesting keyframe (\(reason))")
            sendMessage(.keyframeRequest(KeyframeRequest(
                streamEpoch: streamEpoch,
                lastCompleteSeq: lastCompleteSeq,
                reason: reason
            )))
        }
    }

    /// Fire-and-forget goodbye, sent a few times for loss tolerance.
    func sendBye(_ reason: ByeReason) {
        queue.async { [self] in
            guard sessionId != 0 else { return }
            for delay in [0, 50, 100] {
                queue.asyncAfter(deadline: .now() + .milliseconds(delay)) { [weak self] in
                    self?.sendMessage(.bye(reason))
                }
            }
        }
    }

    private func startPinging() {
        burstPingsRemaining = Self.pingBurstCount
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(Self.pingBurstSpacingMs))
        timer.setEventHandler { [weak self] in self?.pingTick() }
        pingTimer?.cancel()
        pingTimer = timer
        timer.resume()
    }

    private func pingTick() {
        guard sessionId != 0 else { return }
        sendMessage(.ping(WirePing(t1Us: Self.clientNowUs())))
        if burstPingsRemaining > 0 {
            burstPingsRemaining -= 1
            if burstPingsRemaining == 0 {
                // Burst done: settle into the steady cadence.
                pingTimer?.schedule(
                    deadline: .now() + .milliseconds(Self.pingIntervalMs),
                    repeating: .milliseconds(Self.pingIntervalMs)
                )
            }
        }
    }

    func stop() {
        queue.async { [self] in
            helloTimer?.cancel()
            helloTimer = nil
            reportTimer?.cancel()
            reportTimer = nil
            pingTimer?.cancel()
            pingTimer = nil
            sessionId = 0
            estimator.reset()
            lastHeartbeatAtUs.withLock { $0 = 0 }
            clockSnapshot.withLock { $0 = ClockSnapshot() }
        }
    }

    private func sendMessage(_ message: ControlMessage) {
        msgSeq &+= 1
        send(Wire.encodeControl(sessionId: sessionId, msgSeq: msgSeq, message: message))
    }
}
