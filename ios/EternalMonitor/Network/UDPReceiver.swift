import Foundation
import Network

/// Receives UDP datagrams on a specified port, parses FragmentHeader, and feeds fragments
/// to the FrameAssembler. Uses Network.framework NWListener for UDP.
final class UDPReceiver {
    let port: UInt16
    var assembler: FrameAssembler?
    var onConnectionEstablished: (() -> Void)?
    var onError: ((String) -> Void)?

    private var listener: NWListener?
    private var activeConnection: NWConnection?
    private var helloConnection: NWConnection?
    private let queue = DispatchQueue(label: "com.eternal.udp", qos: .userInteractive)
    private var expectedRemoteHost: String?
    private var didEstablishConnection = false

    init(port: UInt16) {
        self.port = port
    }

    /// Start listening for UDP datagrams from `host` and send a HELLO registration
    /// so the host knows where to stream frames.
    @discardableResult
    func start(host: String) -> Bool {
        expectedRemoteHost = Self.normalizeHost(host)
        didEstablishConnection = false

        let params = NWParameters.udp
        params.allowLocalEndpointReuse = true

        do {
            listener = try NWListener(using: params)
        } catch {
            print("[UDPReceiver] Failed to create listener: \(error)")
            onError?("Failed to start UDP listener: \(error.localizedDescription)")
            return false
        }

        listener?.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                let actualPort = self.listener?.port?.rawValue ?? self.port
                print("[UDPReceiver] Listening on port \(actualPort)")
                // Send HELLO to the host so it knows our address
                self.sendHello(to: host)
            case .failed(let error):
                print("[UDPReceiver] Listener failed: \(error)")
                self.onError?("UDP listener failed: \(error.localizedDescription)")
                self.listener?.cancel()
            default:
                break
            }
        }

        listener?.newConnectionHandler = { [weak self] connection in
            guard let self else { return }
            guard self.shouldAccept(connection: connection) else {
                connection.cancel()
                return
            }

            self.activeConnection?.cancel()
            self.activeConnection = connection

            connection.stateUpdateHandler = { state in
                if case .ready = state, !self.didEstablishConnection {
                    self.didEstablishConnection = true
                    self.onConnectionEstablished?()
                }
            }

            connection.start(queue: self.queue)
            self.receiveLoop(on: connection)
        }

        listener?.start(queue: queue)
        return true
    }

    private static let helloMagic = "ETERNALHELLO".data(using: .utf8)!

    /// Send a HELLO registration packet to the host so it streams frames to us.
    /// Sends multiple times to handle packet loss.
    private func sendHello(to host: String) {
        let endpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host(host),
            port: NWEndpoint.Port(rawValue: port)!
        )
        let conn = NWConnection(to: endpoint, using: .udp)
        conn.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            if case .ready = state {
                // Send HELLO a few times to be safe
                for i in 0..<3 {
                    let delay = DispatchTime.now() + .milliseconds(i * 200)
                    self.queue.asyncAfter(deadline: delay) {
                        conn.send(content: Self.helloMagic, completion: .contentProcessed { error in
                            if let error {
                                print("[UDPReceiver] HELLO send error: \(error)")
                            } else {
                                print("[UDPReceiver] HELLO sent to \(host):\(self.port)")
                            }
                        })
                    }
                }
            }
        }
        conn.start(queue: queue)
        helloConnection = conn
    }

    func stop() {
        helloConnection?.cancel()
        helloConnection = nil
        activeConnection?.cancel()
        activeConnection = nil
        listener?.cancel()
        listener = nil
        expectedRemoteHost = nil
        didEstablishConnection = false
    }

    // MARK: - Receive loop

    private func receiveLoop(on connection: NWConnection) {
        connection.receiveMessage { [weak self] content, _, isComplete, error in
            guard let self else { return }

            if let error {
                print("[UDPReceiver] Receive error: \(error)")
                return
            }

            if let data = content, data.count >= FragmentHeader.size {
                self.handleDatagram(data)
            }

            // Continue receiving
            self.receiveLoop(on: connection)
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
            payload: payload
        )
    }

    private func shouldAccept(connection: NWConnection) -> Bool {
        guard let expectedRemoteHost else { return true }
        guard case .hostPort(let host, _) = connection.endpoint else { return false }
        return Self.normalizeHost(host.debugDescription) == expectedRemoteHost
    }

    private static func normalizeHost(_ host: String) -> String {
        host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
    }
}

// MARK: - FragmentHeader

/// Wire format (little-endian, 12 bytes):
///   [0..4]  seq: u32
///   [4]     fragment_index: u8
///   [5]     fragment_count: u8
///   [6..8]  reserved (zero)
///   [8..12] payload_len: u32
struct FragmentHeader {
    static let size = 12

    let seq: UInt32
    let fragmentIndex: UInt8
    let fragmentCount: UInt8
    let payloadLen: UInt32

    init?(data: Data) {
        guard data.count >= Self.size else { return nil }
        seq = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 0, as: UInt32.self).littleEndian }
        fragmentIndex = data[4]
        fragmentCount = data[5]
        // bytes 6-7 reserved
        payloadLen = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 8, as: UInt32.self).littleEndian }
    }
}
