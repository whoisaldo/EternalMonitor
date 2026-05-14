// NEEDS_XCODE_VERIFY: Connection quality tracker — measures rolling packet loss and
// inter-frame jitter. Owned by ConnectionManager; updated from network/decoder
// callbacks. Published @MainActor so SwiftUI can observe.

import Foundation
import QuartzCore

@MainActor
final class ConnectionQualityTracker: ObservableObject {
    @Published var lossPercent: Double = 0
    @Published var jitterMs: Double = 0
    @Published var seqGap: Int = 0

    private var lastSeq: UInt32 = 0
    private var seenAnySeq = false
    private var receivedCount: Int = 0
    private var expectedCount: Int = 0
    private var lossWindowStart: CFTimeInterval = CACurrentMediaTime()

    private var frameTimestamps: [CFTimeInterval] = []
    private let frameWindow = 60

    func recordFragmentSeq(_ seq: UInt32) {
        if !seenAnySeq {
            seenAnySeq = true
            lastSeq = seq
            receivedCount = 1
            expectedCount = 1
            return
        }

        if seq > lastSeq {
            let delta = Int(seq &- lastSeq)
            expectedCount += delta
            receivedCount += 1
            seqGap = max(seqGap, delta - 1)
            lastSeq = seq
        } else {
            // Reordered or duplicate fragment — count as received but don't move lastSeq backwards.
            receivedCount += 1
        }

        let now = CACurrentMediaTime()
        if now - lossWindowStart > 5.0 {
            // Roll the window: halve counters to decay old data while preserving signal.
            receivedCount /= 2
            expectedCount /= 2
            lossWindowStart = now
        }
        if expectedCount > 0 {
            let loss = Double(expectedCount - receivedCount) / Double(expectedCount)
            lossPercent = max(0.0, min(100.0, loss * 100.0))
        }
    }

    func recordFrameDecoded() {
        let now = CACurrentMediaTime()
        frameTimestamps.append(now)
        if frameTimestamps.count > frameWindow {
            frameTimestamps.removeFirst(frameTimestamps.count - frameWindow)
        }
        guard frameTimestamps.count >= 4 else {
            jitterMs = 0
            return
        }
        var deltas: [Double] = []
        deltas.reserveCapacity(frameTimestamps.count - 1)
        for i in 1..<frameTimestamps.count {
            deltas.append((frameTimestamps[i] - frameTimestamps[i - 1]) * 1000.0)
        }
        let mean = deltas.reduce(0, +) / Double(deltas.count)
        let variance = deltas.reduce(0.0) { acc, d in acc + (d - mean) * (d - mean) } / Double(deltas.count)
        jitterMs = variance.squareRoot()
    }

    func reset() {
        lastSeq = 0
        seenAnySeq = false
        receivedCount = 0
        expectedCount = 0
        seqGap = 0
        lossWindowStart = CACurrentMediaTime()
        frameTimestamps.removeAll()
        lossPercent = 0
        jitterMs = 0
    }
}

extension ConnectionQualityTracker {
    /// 1–4 bars based on the brief's thresholds.
    var bars: Int {
        if lossPercent < 1.0 && jitterMs < 5.0 {
            return 4
        }
        if lossPercent < 3.0 && jitterMs < 10.0 {
            return 3
        }
        if lossPercent < 10.0 && jitterMs < 20.0 {
            return 2
        }
        return 1
    }
}
