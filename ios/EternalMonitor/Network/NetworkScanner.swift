import Foundation
import Network

/// Discovered host on the local network.
struct DiscoveredHost: Identifiable, Equatable {
    var id: String { address }
    let name: String
    let address: String
    let port: UInt16
}

/// Scans the local network for EternalMonitor hosts using Bonjour service discovery.
/// The desktop sender does not currently advertise this service, so scanning fails
/// safely instead of guessing across the subnet and returning false positives.
@MainActor
final class NetworkScanner: ObservableObject {
    @Published var hosts: [DiscoveredHost] = []
    @Published var isScanning = false
    @Published var statusMessage: String = ""

    private var browser: NWBrowser?
    private var scanTask: Task<Void, Never>?

    func startScan() {
        guard !isScanning else { return }
        isScanning = true
        hosts = []
        statusMessage = "Scanning network..."

        startBonjourBrowse()

        scanTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            if !Task.isCancelled {
                stopScan()
            }
        }
    }

    func stopScan() {
        browser?.cancel()
        browser = nil
        scanTask?.cancel()
        scanTask = nil
        isScanning = false

        if hosts.isEmpty {
            statusMessage = "Auto-discovery unavailable until the host advertises itself. Enter the host IP manually."
        } else {
            statusMessage = "Found \(hosts.count) host\(hosts.count == 1 ? "" : "s")"
        }
    }

    // MARK: - Bonjour

    private func startBonjourBrowse() {
        let descriptor = NWBrowser.Descriptor.bonjour(type: "_eternaldisplay._udp", domain: nil)
        let browser = NWBrowser(for: descriptor, using: .udp)

        browser.browseResultsChangedHandler = { [weak self] results, _ in
            Task { @MainActor in
                guard let self else { return }
                self.hosts = results.compactMap(Self.discoveredHost(from:))
                if !self.hosts.isEmpty {
                    self.statusMessage = "Found \(self.hosts.count) host\(self.hosts.count == 1 ? "" : "s")"
                }
            }
        }

        browser.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            if case .failed(let error) = state {
                Task { @MainActor in
                    self.statusMessage = "Scan failed: \(error.localizedDescription)"
                    self.stopScan()
                }
            }
        }

        browser.start(queue: .main)
        self.browser = browser
    }

    private static func discoveredHost(from result: NWBrowser.Result) -> DiscoveredHost? {
        switch result.endpoint {
        case .service(let name, _, _, _):
            return DiscoveredHost(name: name, address: name, port: 9876)
        case .hostPort(let host, let port):
            let address = host.debugDescription
            return DiscoveredHost(name: address, address: address, port: port.rawValue)
        default:
            return nil
        }
    }
}
