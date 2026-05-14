import Foundation

/// Discovered host on the local network.
struct DiscoveredHost: Identifiable, Equatable {
    var id: String { "\(name)|\(address)|\(port)" }
    let name: String
    let address: String
    let port: UInt16
}

/// Scans the local network for EternalMonitor hosts using Bonjour service discovery.
@MainActor
final class NetworkScanner: NSObject, ObservableObject {
    @Published var hosts: [DiscoveredHost] = []
    @Published var isScanning = false
    @Published var statusMessage: String = ""

    private let browser = NetServiceBrowser()
    private var resolvingServices: [String: NetService] = [:]
    private var scanTask: Task<Void, Never>?
    // NEEDS_XCODE_VERIFY: track auto-retry state so we don't bounce forever when discovery is broken.
    private var didAutoRetry = false

    override init() {
        super.init()
        browser.delegate = self
    }

    func startScan() {
        guard !isScanning else { return }
        didAutoRetry = false
        beginScan()
    }

    private func beginScan() {
        isScanning = true
        hosts = []
        resolvingServices.removeAll()
        statusMessage = "Scanning network..."

        browser.searchForServices(ofType: "_eternaldisplay._udp.", inDomain: "local.")

        scanTask = Task { @MainActor in
            // NEEDS_XCODE_VERIFY: 10s timeout — many routers are slow to forward the first mDNS
            // response, especially right after Wi-Fi reconnect. The brief raised this from 5s.
            try? await Task.sleep(nanoseconds: 10_000_000_000)
            if !Task.isCancelled {
                stopScan()
            }
        }
    }

    func stopScan() {
        browser.stop()
        resolvingServices.values.forEach { service in
            service.stop()
            service.delegate = nil
        }
        resolvingServices.removeAll()
        scanTask?.cancel()
        scanTask = nil
        isScanning = false

        if hosts.isEmpty {
            if !didAutoRetry {
                // NEEDS_XCODE_VERIFY: auto-retry once after 2s. Bonjour browse occasionally
                // misses the first multicast burst.
                didAutoRetry = true
                statusMessage = "No hosts found yet — retrying..."
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    if self.hosts.isEmpty && !self.isScanning {
                        self.beginScan()
                    }
                }
            } else {
                statusMessage = "No hosts found. Make sure the Windows host is running on the same network."
            }
        } else {
            statusMessage = "Found \(hosts.count) host\(hosts.count == 1 ? "" : "s")"
        }
    }
}

extension NetworkScanner: NetServiceBrowserDelegate, NetServiceDelegate {
    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didFind service: NetService,
        moreComing: Bool
    ) {
        Task { @MainActor in
            let key = Self.serviceKey(service)
            guard resolvingServices[key] == nil else { return }

            service.delegate = self
            resolvingServices[key] = service
            service.resolve(withTimeout: 4)

            if !moreComing {
                statusMessage = "Resolving hosts..."
            }
        }
    }

    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didRemove service: NetService,
        moreComing: Bool
    ) {
        Task { @MainActor in
            let key = Self.serviceKey(service)
            resolvingServices.removeValue(forKey: key)
            hosts.removeAll { $0.name == service.name }

            if !moreComing {
                statusMessage = hosts.isEmpty
                    ? "No hosts found. Make sure the Windows host is running on the same network."
                    : "Found \(hosts.count) host\(hosts.count == 1 ? "" : "s")"
            }
        }
    }

    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didNotSearch errorDict: [String: NSNumber]
    ) {
        Task { @MainActor in
            statusMessage = "Scan failed. Local network permission may be blocked."
            stopScan()
        }
    }

    nonisolated func netServiceDidResolveAddress(_ sender: NetService) {
        Task { @MainActor in
            let key = Self.serviceKey(sender)
            defer { resolvingServices.removeValue(forKey: key) }

            guard let host = Self.discoveredHost(from: sender) else { return }
            // NEEDS_XCODE_VERIFY: discovery diagnostic — surface every resolved host so we
            // can tell whether mDNS is finding the right machine.
            print("[mDNS] Found: \(host.name) at \(host.address):\(host.port)")
            hosts.removeAll { $0.name == host.name || $0.address == host.address }
            hosts.append(host)
            hosts.sort(by: Self.hostQualityCompare)
            statusMessage = "Found \(hosts.count) host\(hosts.count == 1 ? "" : "s")"
        }
    }

    /// Prefer IPv4 over IPv6, then prefer same-subnet over different-subnet, then alpha by name.
    /// NEEDS_XCODE_VERIFY
    private static func hostQualityCompare(_ a: DiscoveredHost, _ b: DiscoveredHost) -> Bool {
        let aV4 = a.address.contains(".")
        let bV4 = b.address.contains(".")
        if aV4 != bV4 {
            return aV4 // IPv4 first
        }
        let ourSubnet = primaryIPv4Subnet()
        if let prefix = ourSubnet {
            let aSame = a.address.hasPrefix(prefix)
            let bSame = b.address.hasPrefix(prefix)
            if aSame != bSame {
                return aSame
            }
        }
        return a.name.localizedCaseInsensitiveCompare(b.name) == .orderedAscending
    }

    /// Best-effort: return the first three octets ("192.168.1.") of the iPad's primary
    /// non-loopback IPv4 interface, so we can prefer same-subnet hosts when sorting.
    /// Returns nil on failure. NEEDS_XCODE_VERIFY.
    private static func primaryIPv4Subnet() -> String? {
        var ifaddr: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifaddr) == 0, let first = ifaddr else { return nil }
        defer { freeifaddrs(ifaddr) }

        var ptr: UnsafeMutablePointer<ifaddrs>? = first
        while let cur = ptr {
            let flags = Int32(cur.pointee.ifa_flags)
            let family = cur.pointee.ifa_addr.pointee.sa_family
            if (flags & IFF_UP) != 0 && (flags & IFF_LOOPBACK) == 0 && family == AF_INET {
                var hostname = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                let result = getnameinfo(
                    cur.pointee.ifa_addr,
                    socklen_t(cur.pointee.ifa_addr.pointee.sa_len),
                    &hostname,
                    socklen_t(hostname.count),
                    nil,
                    socklen_t(0),
                    NI_NUMERICHOST
                )
                if result == 0 {
                    let ip = String(cString: hostname)
                    let parts = ip.split(separator: ".")
                    if parts.count == 4 {
                        return "\(parts[0]).\(parts[1]).\(parts[2])."
                    }
                }
            }
            ptr = cur.pointee.ifa_next
        }
        return nil
    }

    nonisolated func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) {
        Task { @MainActor in
            resolvingServices.removeValue(forKey: Self.serviceKey(sender))
            if hosts.isEmpty {
                statusMessage = "Resolving hosts..."
            }
        }
    }

    private static func serviceKey(_ service: NetService) -> String {
        "\(service.name)|\(service.type)|\(service.domain)"
    }

    private static func discoveredHost(from service: NetService) -> DiscoveredHost? {
        guard let addresses = service.addresses else { return nil }

        for data in addresses {
            if let address = ipAddress(from: data, family: AF_INET) {
                let port = service.port > 0 ? UInt16(service.port) : 9876
                return DiscoveredHost(name: service.name, address: address, port: port)
            }
        }

        for data in addresses {
            if let address = ipAddress(from: data) {
                let port = service.port > 0 ? UInt16(service.port) : 9876
                return DiscoveredHost(name: service.name, address: address, port: port)
            }
        }

        return nil
    }

    private static func ipAddress(from data: Data, family expectedFamily: Int32? = nil) -> String? {
        data.withUnsafeBytes { rawBuffer in
            guard let base = rawBuffer.baseAddress else { return nil }
            let sockaddr = base.assumingMemoryBound(to: sockaddr.self)
            let family = Int32(sockaddr.pointee.sa_family)

            if let expectedFamily, family != expectedFamily {
                return nil
            }

            switch family {
            case AF_INET:
                var addr = base.assumingMemoryBound(to: sockaddr_in.self).pointee.sin_addr
                var buffer = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                guard inet_ntop(AF_INET, &addr, &buffer, socklen_t(buffer.count)) != nil else {
                    return nil
                }
                return String(cString: buffer)
            case AF_INET6:
                var addr = base.assumingMemoryBound(to: sockaddr_in6.self).pointee.sin6_addr
                var buffer = [CChar](repeating: 0, count: Int(INET6_ADDRSTRLEN))
                guard inet_ntop(AF_INET6, &addr, &buffer, socklen_t(buffer.count)) != nil else {
                    return nil
                }
                return String(cString: buffer)
            default:
                return nil
            }
        }
    }
}
