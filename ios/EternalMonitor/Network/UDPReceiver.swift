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
    // surface the sequence number of every fragment so the
    // ConnectionQualityTracker can compute rolling packet loss.
    var onFragmentSeq: ((UInt32) -> Void)?

    private var connection: NWConnection?
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
        params.requiredLocalEndpoint = NWEndpoint.hostPort(
            host: .ipv4(.any),
            port: NWEndpoint.Port(rawValue: port)!
        )

        let endpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host(host),
            port: NWEndpoint.Port(rawValue: port)!
        )

        let connection = NWConnection(to: endpoint, using: params)
        connection.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                let actualPort = self.port
                print("[UDPReceiver] Listening on port \(actualPort)")
                self.onListenerReady?(actualPort)
                self.sendHello(to: host)
                if !self.didEstablishConnection {
                    self.didEstablishConnection = true
                    self.onConnectionEstablished?()
                }
                self.receiveLoop()
            case .failed(let error):
                print("[UDPReceiver] Connection failed: \(error)")
                self.onError?("UDP connection failed: \(error.localizedDescription)")
                connection.cancel()
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

    private static let helloMagic = "ETERNALHELLO".data(using: .utf8)!

    /// Build HELLO payload: magic bytes + 2-byte LE listening port.
    private func helloPayload() -> Data {
        var data = Self.helloMagic
        var lePort = port.littleEndian
        data.append(Data(bytes: &lePort, count: 2))
        return data
    }

    /// Send a HELLO registration packet to the host so it streams frames to us.
    /// Sends multiple times to handle packet loss.
    private func sendHello(to host: String) {
        guard let connection else {
            onHelloFailure?("UDP connection not available")
            return
        }
        let payload = helloPayload()
        let attempts = 3
        for i in 0..<attempts {
            let delay = DispatchTime.now() + .milliseconds(i * 200)
            queue.asyncAfter(deadline: delay) { [weak self] in
                guard let self else { return }
                self.onHelloAttempt?(i + 1, attempts, host, self.port)
                connection.send(content: payload, completion: .contentProcessed { error in
                    if let error {
                        print("[UDPReceiver] HELLO send error: \(error)")
                        self.onHelloFailure?(error.localizedDescription)
                    } else {
                        print("[UDPReceiver] HELLO sent to \(host):\(self.port)")
                    }
                })
            }
        }
    }

    func stop() {
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
                self.onError?("UDP receive error: \(error.localizedDescription)")
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

        onFragmentSeq?(header.seq)
        let payload = data.subdata(in: payloadStart..<payloadEnd)
        assembler?.addFragment(
            seq: header.seq,
            index: header.fragmentIndex,
            count: header.fragmentCount,
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
///   [12..16] reserved (zero)
struct FragmentHeader {
    static let size = 16

    let seq: UInt32
    let fragmentIndex: UInt16
    let fragmentCount: UInt16
    let payloadLen: UInt32

    init?(data: Data) {
        guard data.count >= Self.size else { return nil }
        seq = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 0, as: UInt32.self).littleEndian }
        fragmentIndex = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 4, as: UInt16.self).littleEndian }
        fragmentCount = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 6, as: UInt16.self).littleEndian }
        payloadLen = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 8, as: UInt32.self).littleEndian }
    }
}
