import Foundation
import Network

/// Receives UDP datagrams on a specified port, parses FragmentHeader, and feeds fragments
/// to the FrameAssembler. Uses Network.framework NWListener for UDP.
final class UDPReceiver {
    let port: UInt16
    var assembler: FrameAssembler?
    var onConnectionEstablished: (() -> Void)?

    private var listener: NWListener?
    private var activeConnection: NWConnection?
    private let queue = DispatchQueue(label: "com.eternal.udp", qos: .userInteractive)

    init(port: UInt16) {
        self.port = port
    }

    /// Start listening for UDP datagrams. `host` is informational only — we listen on all interfaces.
    func start(host: String) {
        let params = NWParameters.udp
        params.allowLocalEndpointReuse = true
        params.requiredLocalEndpoint = NWEndpoint.hostPort(
            host: .ipv4(.any),
            port: NWEndpoint.Port(rawValue: port)!
        )

        do {
            listener = try NWListener(using: params)
        } catch {
            print("[UDPReceiver] Failed to create listener: \(error)")
            return
        }

        listener?.stateUpdateHandler = { state in
            switch state {
            case .ready:
                print("[UDPReceiver] Listening on port \(self.port)")
            case .failed(let error):
                print("[UDPReceiver] Listener failed: \(error)")
                self.listener?.cancel()
            default:
                break
            }
        }

        listener?.newConnectionHandler = { [weak self] connection in
            guard let self else { return }
            // Accept the first connection (the Windows host)
            // Cancel any previous connection
            self.activeConnection?.cancel()
            self.activeConnection = connection

            connection.stateUpdateHandler = { state in
                if case .ready = state {
                    self.onConnectionEstablished?()
                }
            }

            connection.start(queue: self.queue)
            self.receiveLoop(on: connection)
        }

        listener?.start(queue: queue)
    }

    func stop() {
        activeConnection?.cancel()
        activeConnection = nil
        listener?.cancel()
        listener = nil
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
