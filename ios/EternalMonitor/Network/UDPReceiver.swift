import Foundation
import Network
import os

/// One UDP socket to the host: media fragments in, control datagrams both
/// ways. Binds an EPHEMERAL local port (advertised to the host in HELLO2);
/// demuxes inbound traffic by the v2 wire prefix — media goes to the
/// `FrameAssembler`, control to the `ControlChannel`.
final class UDPReceiver {
    /// The host's control/media port we connect to.
    let port: UInt16
    var assembler: FrameAssembler?
    var onControlDatagram: ((Data) -> Void)?
    var onConnectionEstablished: (() -> Void)?
    var onError: ((String) -> Void)?
    var onListenerReady: ((UInt16) -> Void)?
    var onDatagramReceived: ((Int) -> Void)?
    var onDatagramIgnored: ((String) -> Void)?

    /// Media datagrams must carry this session id (set after HELLO_ACK);
    /// 0 = no session yet, drop all media.
    private let acceptedSessionId = OSAllocatedUnfairLock<UInt32>(initialState: 0)
    /// Count of v1-shaped datagrams (legacy host detection): 16+ bytes that
    /// classify as neither v2 nor the legacy hello.
    private let unknownDatagramCount = OSAllocatedUnfairLock<Int>(initialState: 0)

    private var connection: NWConnection?
    private let queue = DispatchQueue(label: "com.eternal.udp", qos: .userInteractive)

    init(port: UInt16) {
        self.port = port
    }

    var controlQueue: DispatchQueue { queue }

    func setAcceptedSessionId(_ id: UInt32) {
        acceptedSessionId.withLock { $0 = id }
    }

    /// v1-shaped datagrams observed (used to tell "old host" apart from "no host").
    var legacyLookingDatagrams: Int {
        unknownDatagramCount.withLock { $0 }
    }

    /// Start the socket toward `host`. HELLO2 is the ControlChannel's job once
    /// `onListenerReady` reports the ephemeral local port.
    @discardableResult
    func start(host: String) -> Bool {
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
                print("[UDPReceiver] Socket ready, ephemeral port \(actualPort)")
                self.onListenerReady?(actualPort)
                self.onConnectionEstablished?()
                self.receiveLoop()
            case .waiting(let error):
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

    /// Send one datagram to the host (control plane). Safe from any thread.
    func send(_ data: Data) {
        connection?.send(content: data, completion: .contentProcessed { _ in })
    }

    /// The ephemeral local port the OS assigned to this connection's socket.
    private func localPort() -> UInt16? {
        guard case .hostPort(_, let port)? = connection?.currentPath?.localEndpoint else {
            return nil
        }
        return port.rawValue
    }

    func stop() {
        // Break the handler → connection reference before cancel so each
        // connect/disconnect cycle can actually deallocate the NWConnection.
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
        acceptedSessionId.withLock { $0 = 0 }
        unknownDatagramCount.withLock { $0 = 0 }
    }

    // MARK: - Receive loop

    private func receiveLoop() {
        guard let connection else { return }
        connection.receiveMessage { [weak self] content, _, _, error in
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

            if let data = content {
                self.handleDatagram(data)
            }

            // Continue receiving
            self.receiveLoop()
        }
    }

    private func handleDatagram(_ data: Data) {
        switch Wire.classify(data) {
        case .media:
            onDatagramReceived?(data.count)
            guard let (header, payloadRange) = MediaHeader.decode(data) else {
                onDatagramIgnored?("Dropped malformed media datagram (\(data.count) bytes)")
                return
            }
            let expected = acceptedSessionId.withLock { $0 }
            guard header.sessionId == expected, expected != 0 else {
                onDatagramIgnored?("Dropped media for foreign session \(header.sessionId)")
                return
            }
            assembler?.addFragment(
                seq: header.frameSeq,
                index: header.fragIndex,
                count: header.fragCount,
                epoch: header.streamEpoch,
                isKeyframe: header.isKeyframe,
                captureTimestampUs: header.captureTimestampUs,
                payload: data.subdata(in: payloadRange)
            )
        case .control:
            onControlDatagram?(data)
        case .legacyHello:
            // The host never sends this; ignore.
            break
        case .unknown:
            if data.count >= 16 {
                // Looks like v1 media from an old host — count it so the app
                // can say "update the Windows host" instead of "no host found".
                unknownDatagramCount.withLock { $0 += 1 }
            }
            onDatagramIgnored?("Ignored unrecognized datagram (\(data.count) bytes)")
        }
    }
}
