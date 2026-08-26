import Foundation
import os

/// Machine-readable milestones for the automated end-to-end harness
/// (`scripts/e2e_ios.sh`), scraped from the simulator via `log stream` on the
/// `com.eternal.monitor.e2e` subsystem. Enabled only when the app is launched
/// with `EM_E2E_LOG=1`; completely silent otherwise.
enum E2E {
    static let enabled = ProcessInfo.processInfo.environment["EM_E2E_LOG"] == "1"
    static let logger = Logger(subsystem: "com.eternal.monitor.e2e", category: "milestone")

    /// `EM_AUTOCONNECT=host:port` makes the app connect immediately on launch,
    /// bypassing the connect screen — the harness's way in.
    static var autoconnectTarget: (host: String, port: UInt16)? {
        guard let spec = ProcessInfo.processInfo.environment["EM_AUTOCONNECT"],
              let colon = spec.lastIndex(of: ":"),
              let port = UInt16(spec[spec.index(after: colon)...])
        else { return nil }
        let host = String(spec[..<colon])
        return host.isEmpty ? nil : (host, port)
    }

    static func firstFrame(width: Int, height: Int) {
        guard enabled else { return }
        logger.log("E2E_FIRST_FRAME w=\(width, privacy: .public) h=\(height, privacy: .public)")
    }

    static func stats(decoded: Int, width: Int, height: Int, fps: Double) {
        guard enabled else { return }
        logger.log(
            "E2E_STATS decoded=\(decoded, privacy: .public) w=\(width, privacy: .public) h=\(height, privacy: .public) fps=\(Int(fps), privacy: .public)"
        )
    }
}
