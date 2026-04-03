import Foundation

/// Reassembles fragmented UDP datagrams into complete FlatBuffer payloads.
/// Called exclusively from the UDP receiver's serial queue — no locking needed.
final class FrameAssembler {
    var onFrameAssembled: ((Data) -> Void)?

    private var pending: [UInt32: PendingFrame] = [:]
    private var latestCompletedSeq: UInt32 = 0
    private var cleanupCounter: UInt32 = 0

    struct PendingFrame {
        let fragmentCount: UInt8
        var fragments: [UInt8: Data]
        let createdAt: UInt64  // mach_absolute_time

        var isComplete: Bool {
            fragments.count == Int(fragmentCount)
        }
    }

    func addFragment(seq: UInt32, index: UInt8, count: UInt8, payload: Data) {
        // Drop stale fragments (seq older than latest completed)
        if seq <= latestCompletedSeq && latestCompletedSeq > 0 {
            return
        }

        // Get or create pending frame
        if pending[seq] == nil {
            pending[seq] = PendingFrame(
                fragmentCount: count,
                fragments: [:],
                createdAt: mach_absolute_time()
            )
        }

        pending[seq]?.fragments[index] = payload

        // Check if frame is complete
        if let frame = pending[seq], frame.isComplete {
            // Reassemble in fragment index order
            var assembled = Data()
            for i in 0..<frame.fragmentCount {
                if let fragment = frame.fragments[i] {
                    assembled.append(fragment)
                }
            }

            latestCompletedSeq = seq
            pending.removeValue(forKey: seq)

            // Evict any frames older than the completed one
            pending = pending.filter { $0.key > seq }

            onFrameAssembled?(assembled)
        }

        // Periodic cleanup of stale entries
        cleanupCounter += 1
        if cleanupCounter % 100 == 0 {
            evictStale()
        }
    }

    func reset() {
        pending.removeAll()
        latestCompletedSeq = 0
        cleanupCounter = 0
    }

    private func evictStale() {
        let now = mach_absolute_time()
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        let numer = UInt64(info.numer)
        let denom = UInt64(info.denom)

        pending = pending.filter { _, frame in
            let elapsedNs = ((now - frame.createdAt) * numer) / denom
            return elapsedNs < 100_000_000  // 100ms timeout
        }
    }
}
