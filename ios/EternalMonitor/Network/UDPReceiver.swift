import Foundation
import Network

/// Receives UDP datagrams on a specified port, parses FragmentHeader, and feeds fragments
/// to the FrameAssembler. Uses a single UDP connection bound to the local listening port
/// so HELLO registration and frame reception happen on the same socket.
final class UDPReceiver {
    let port: UInt16
    var assembler: FrameAssembler?
    var onConnectionEstablished: (() -> Void)?
    var onError: ((String) -> Void)?
    var onListenerReady: ((UInt16) -> Void)?
    var onHelloAttempt: ((Int, Int, String, UInt16) -> Void)?
    var onHelloFailure: ((String) -> Void)?
    var onDatagramReceived: ((Int) -> Void)?
    var onDatagramIgnored: ((String) -> Void)?

    private var connection: NWConnection?
    private let queue = DispatchQueue(label: "com.eternal.udp", qos: .userInteractive)
    private var expectedRemoteHost: String?
    private var didEstablishConnection = false

    init(port: UInt16) {
        self.port = port
    }

    /// Start listening for UDP datagrams from `host` and send a HELLO registration
    /// so the host knows where to stream frames.
    ///
    /// The local port is EPHEMERAL (the OS picks it) and advertised to the host
    /// inside the HELLO payload. Binding to the host's port — the old behavior —
    /// gained nothing (the host streams to whatever port HELLO names) and made
    /// the bind collide with anything else on that port, including the host
    /// itself when both run on one machine (the simulator E2E).
    @discardableResult
    func start(host: String) -> Bool {
        expectedRemoteHost = Self.normalizeHost(host)
        didEstablishConnection = false

        let params = NWParameters.udp

        guard let remotePort = NWEndpoint.Port(rawValue: port) else {
            onError?("Invalid port \(port)")
            return false
        }
        let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(host), port: remotePort)

        let connection = NWConnection(to: endpoint, using: params)
        connection.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                let actualPort = self.localPort() ?? 0
                print("[UDPReceiver] Listening on ephemeral port \(actualPort)")
                self.onListenerReady?(actualPort)
                self.sendHello(to: host, listeningOn: actualPort)
                if !self.didEstablishConnection {
                    self.didEstablishConnection = true
                    self.onConnectionEstablished?()
                }
                self.receiveLoop()
            case .waiting(let error):
                // Path not viable yet (permission prompt pending, interface down).
                print("[UDPReceiver] Connection waiting: \(error)")
                self.onError?("UDP path not ready: \(error.localizedDescription)")
            case .failed(let error):
                print("[UDPReceiver] Connection failed: \(error)")
                self.onError?("UDP connection failed: \(error.localizedDescription)")
                self.connection?.cancel()
            case .cancelled:
                break
            default:
                break
            }
        }

        self.connection = connection
        connection.start(queue: queue)
        return true
    }

    /// The ephemeral local port the OS assigned to this connection's socket.
    private func localPort() -> UInt16? {
        guard case .hostPort(_, let port)? = connection?.currentPath?.localEndpoint else {
            return nil
        }
        return port.rawValue
    }

    private static let helloMagic = "ETERNALHELLO".data(using: .utf8)!

    /// Build HELLO payload: magic bytes + 2-byte LE listening port.
    private func helloPayload(listeningOn listenPort: UInt16) -> Data {
        var data = Self.helloMagic
        var lePort = listenPort.littleEndian
        data.append(Data(bytes: &lePort, count: 2))
        return data
    }

    /// Send a HELLO registration packet to the host so it streams frames to us.
    /// Sends multiple times to handle packet loss.
    private func sendHello(to host: String, listeningOn listenPort: UInt16) {
        guard connection != nil else {
            onHelloFailure?("UDP connection not available")
            return
        }
        let payload = helloPayload(listeningOn: listenPort)
        let attempts = 3
        for i in 0..<attempts {
            let delay = DispatchTime.now() + .milliseconds(i * 200)
            queue.asyncAfter(deadline: delay) { [weak self] in
                guard let self, let connection = self.connection else { return }
                self.onHelloAttempt?(i + 1, attempts, host, self.port)
                connection.send(content: payload, completion: .contentProcessed { [weak self] error in
                    if let error {
                        print("[UDPReceiver] HELLO send error: \(error)")
                        self?.onHelloFailure?(error.localizedDescription)
                    } else {
                        print("[UDPReceiver] HELLO sent to \(host) advertising port \(listenPort)")
                    }
                })
            }
        }
    }

    func stop() {
        // Break the handler → connection reference before cancel so each
        // connect/disconnect cycle can actually deallocate the NWConnection.
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
        expectedRemoteHost = nil
        didEstablishConnection = false
    }

    // MARK: - Receive loop

    private func receiveLoop() {
        guard let connection else { return }
        connection.receiveMessage { [weak self] content, _, isComplete, error in
            guard let self else { return }

            if let error {
                print("[UDPReceiver] Receive error: \(error)")
                // A transient error (ICMP port-unreachable while the host
                // restarts its pipeline, a path blip) must not silently kill
                // reception forever — re-arm after a short delay as long as the
                // connection object is still live.
                self.onError?("UDP receive error: \(error.localizedDescription)")
                if self.connection != nil {
                    self.queue.asyncAfter(deadline: .now() + .milliseconds(100)) { [weak self] in
                        self?.receiveLoop()
                    }
                }
                return
            }

            if let data = content, data.count >= FragmentHeader.size {
                self.onDatagramReceived?(data.count)
                self.handleDatagram(data)
            } else if let data = content {
                self.onDatagramIgnored?("Ignored short UDP datagram (\(data.count) bytes)")
            }

            // Continue receiving
            self.receiveLoop()
        }
    }

    private func handleDatagram(_ data: Data) {
        guard let header = FragmentHeader(data: data) else { return }
        let payloadStart = FragmentHeader.size
        let payloadEnd = payloadStart + Int(header.payloadLen)
        guard payloadEnd <= data.count else { return }

        let payload = data.subdata(in: payloadStart..<payloadEnd)
        assembler?.addFragment(
            seq: header.seq,
            index: header.fragmentIndex,
            count: header.fragmentCount,
            epoch: header.streamEpoch,
            payload: payload
        )
    }

    private static func normalizeHost(_ host: String) -> String {
        host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
    }
}

// MARK: - FragmentHeader

/// Wire format (little-endian, 16 bytes):
///   [0..4]  seq: u32
///   [4..6]  fragment_index: u16
///   [6..8]  fragment_count: u16
///   [8..12] payload_len: u32
///   [12..16] stream_epoch: u32  (older hosts send 0 here; treated as "no epoch")
struct FragmentHeader {
    static let size = 16

    let seq: UInt32
    let fragmentIndex: UInt16
    let fragmentCount: UInt16
    let payloadLen: UInt32
    let streamEpoch: UInt32

    init?(data: Data) {
        guard data.count >= Self.size else { return nil }
        seq = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 0, as: UInt32.self).littleEndian }
        fragmentIndex = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 4, as: UInt16.self).littleEndian }
        fragmentCount = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 6, as: UInt16.self).littleEndian }
        payloadLen = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 8, as: UInt32.self).littleEndian }
        streamEpoch = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 12, as: UInt32.self).littleEndian }
    }
}
