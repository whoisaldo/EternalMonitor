import Foundation

/// Parses the `eternaldisplay://host:port` pairing links the host renders as
/// QR codes. Pure and unit-tested — the old inline `split(separator: ":")`
/// broke on IPv6 addresses and trailing slashes.
enum PairingCode {
    enum ParseError: Error, Equatable {
        case wrongScheme
        case missingHost
        case missingOrInvalidPort
    }

    static let scheme = "eternaldisplay"

    static func parse(_ value: String) -> Result<(host: String, port: UInt16), ParseError> {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed),
              components.scheme?.lowercased() == scheme
        else {
            return .failure(.wrongScheme)
        }
        // URLComponents keeps the brackets on IPv6 literals ("[fe80::1]");
        // NWConnection wants the bare address.
        guard var host = components.host, !host.isEmpty else {
            return .failure(.missingHost)
        }
        if host.hasPrefix("["), host.hasSuffix("]") {
            host = String(host.dropFirst().dropLast())
        }
        guard !host.isEmpty else { return .failure(.missingHost) }
        guard let rawPort = components.port, (1...65_535).contains(rawPort) else {
            return .failure(.missingOrInvalidPort)
        }
        return .success((host, UInt16(rawPort)))
    }
}
