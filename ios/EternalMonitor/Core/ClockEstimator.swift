import Foundation

/// NTP-style clock alignment between the host's process clock and this
/// device's uptime clock, fed by PING/PONG exchanges.
///
/// For each exchange: offset = ((t2−t1)+(t3−t4))/2, rtt = (t4−t1)−(t3−t2).
/// The offset in use comes from the MINIMUM-RTT sample in a rolling window —
/// for monotonic clocks over LAN horizons a min-filter beats averaging,
/// because queueing delay only ever inflates RTT (and corrupts the offset
/// symmetrically at most rtt/2).
struct ClockEstimator {
    struct Sample {
        let offsetUs: Int64
        let rttUs: UInt64
        let at: UInt64 // client clock when measured
    }

    /// Samples older than this fall out of the window.
    static let windowUs: UInt64 = 60_000_000
    /// Need at least this many samples before trusting the offset.
    static let minimumSamples = 3

    private var samples: [Sample] = []

    /// Feed one PONG. `t1/t4` are client microseconds, `t2/t3` host microseconds.
    mutating func addExchange(t1: UInt64, t2: UInt64, t3: UInt64, t4: UInt64) {
        guard t4 >= t1, t3 >= t2 else { return } // clock nonsense — drop
        let rtt = (t4 - t1) - min(t3 - t2, t4 - t1)
        let offset = (Int64(bitPattern: t2 &- t1) + Int64(bitPattern: t3 &- t4)) / 2
        samples.append(Sample(offsetUs: offset, rttUs: rtt, at: t4))
        samples.removeAll { t4 - $0.at > Self.windowUs }
        // Bound memory even under ping floods.
        if samples.count > 128 {
            samples.removeFirst(samples.count - 128)
        }
    }

    /// host_time ≈ client_time + offset. Nil until enough samples exist.
    var offsetUs: Int64? {
        guard samples.count >= Self.minimumSamples else { return nil }
        return samples.min { $0.rttUs < $1.rttUs }?.offsetUs
    }

    /// Smoothed round-trip estimate (the window's minimum).
    var rttUs: UInt64? {
        guard !samples.isEmpty else { return nil }
        return samples.map(\.rttUs).min()
    }

    /// End-to-end latency of a frame stamped `hostCaptureUs`, observed at
    /// client time `clientNowUs`. Nil until the offset converges.
    func endToEndLatencyUs(hostCaptureUs: UInt64, clientNowUs: UInt64) -> UInt64? {
        guard let offset = offsetUs else { return nil }
        // capture time in CLIENT clock = hostCaptureUs - offset.
        let captureClient = Int64(bitPattern: hostCaptureUs) - offset
        let latency = Int64(bitPattern: clientNowUs) - captureClient
        return latency > 0 ? UInt64(latency) : 0
    }

    mutating func reset() {
        samples.removeAll()
    }
}
