import Foundation

/// Live stream health for the HUD and receiver reports — real measurements
/// only: loss from the assembler's fragment accounting, RTT/latency from
/// protocol-v2 clock sync. (The old ConnectionQualityTracker computed a loss
/// that was structurally pinned at 0 and a "lag" that subtracted two unrelated
/// clocks.)
struct StreamStats: Equatable {
    var decodeFps: Double = 0
    /// End-to-end capture→display estimate; nil until clock sync converges.
    var e2eMs: Double?
    var rttMs: Double?
    /// Fragment loss over the last stats window.
    var lossPercent: Double = 0
    var framesDropped: UInt64 = 0
    /// 1–4 signal bars for the HUD.
    var bars: Int = 4

    static func bars(lossPercent: Double, rttMs: Double?) -> Int {
        let rtt = rttMs ?? 0
        if lossPercent < 1, rtt < 30 { return 4 }
        if lossPercent < 3, rtt < 60 { return 3 }
        if lossPercent < 10, rtt < 120 { return 2 }
        return 1
    }
}
