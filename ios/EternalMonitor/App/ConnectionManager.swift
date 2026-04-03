import Foundation
import CoreVideo
import QuartzCore
import Combine
import os

enum ConnectionState: Equatable {
    case disconnected
    case connecting
    case connected
}

@MainActor
final class ConnectionManager: ObservableObject {
    @Published var state: ConnectionState = .disconnected
    @Published var fps: Double = 0
    @Published var lagMs: Double = 0
    @Published var transportMode: String = "WiFi"

    private var udpReceiver: UDPReceiver?
    private var frameAssembler: FrameAssembler?
    private var videoDecoder: VideoDecoder?

    private let frameSlot = FrameSlot()
    private var fpsCounter = FPSCounter()

    // MARK: - Frame slot (thread-safe single-slot)

    var latestFrame: CVPixelBuffer? {
        frameSlot.take()
    }

    // MARK: - Connect / Disconnect

    func connect(host: String, port: UInt16) {
        guard state == .disconnected else { return }
        state = .connecting

        let decoder = VideoDecoder()
        let assembler = FrameAssembler()
        let receiver = UDPReceiver(port: port)

        decoder.onFrameDecoded = { [weak self] pixelBuffer, timestampUs in
            guard let self else { return }
            self.frameSlot.set(pixelBuffer)
            Task { @MainActor in
                self.fpsCounter.tick()
                self.fps = self.fpsCounter.currentFPS
                let nowUs = UInt64(ProcessInfo.processInfo.systemUptime * 1_000_000)
                if timestampUs > 0 {
                    // lag is approximate — host and iPad clocks are not synced
                    self.lagMs = Double(nowUs &- timestampUs) / 1000.0
                }
            }
        }

        assembler.onFrameAssembled = { [weak decoder] data in
            guard let decoder, let packet = FramePacket.deserialize(from: data) else { return }
            decoder.decode(packet: packet)
        }

        receiver.assembler = assembler
        receiver.onConnectionEstablished = { [weak self] in
            Task { @MainActor in
                self?.state = .connected
            }
        }

        self.videoDecoder = decoder
        self.frameAssembler = assembler
        self.udpReceiver = receiver

        receiver.start(host: host)

        RecentConnectionStore.shared.add(host: host, port: port, isUSB: false)
    }

    func disconnect() {
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
}

// MARK: - Thread-safe single-slot pixel buffer

final class FrameSlot: @unchecked Sendable {
    private let lock = os.OSAllocatedUnfairLock<CVPixelBuffer?>(initialState: nil)

    func set(_ buffer: CVPixelBuffer) {
        lock.withLock { $0 = buffer }
    }

    func take() -> CVPixelBuffer? {
        lock.withLock { current in
            let frame = current
            current = nil
            return frame
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
        // Keep last 1 second of timestamps
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
