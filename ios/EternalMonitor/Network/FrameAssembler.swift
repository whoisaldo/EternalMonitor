import Foundation

/// Reassembles fragmented UDP datagrams into complete FlatBuffer payloads.
/// Called exclusively from the UDP receiver's serial queue — no locking needed.
final class FrameAssembler {
    var onFrameAssembled: ((Data) -> Void)?
    var onDiagnostic: ((String) -> Void)?

    private var pending: [UInt32: PendingFrame] = [:]
    private var latestCompletedSeq: UInt32 = 0
    private var cleanupCounter: UInt32 = 0
    /// The host stamps a per-pipeline-run `stream_epoch` into each fragment header. When it
    /// changes we know the host restarted (seq reset toward 1) and drop the old stream's state
    /// immediately — more reliable than inferring a restart from a sequence gap. `nil` until the
    /// first fragment; `0` from older hosts that don't set it (those rely on `streamRestartGap`).
    private var currentEpoch: UInt32?

    /// A backward jump in sequence numbers larger than this means the host restarted its
    /// pipeline (capture-display switch, resolution change, etc.) and reset seq toward 0.
    /// Without this, every frame of the new stream is `<= latestCompletedSeq` and gets
    /// dropped forever — the app appears frozen until it's force-quit.
    private static let streamRestartGap: UInt32 = 256

    struct PendingFrame {
        let fragmentCount: UInt16
        var fragments: [UInt16: Data]
        let createdAt: UInt64  // mach_absolute_time

        var isComplete: Bool {
            fragments.count == Int(fragmentCount)
        }
    }

    func addFragment(seq: UInt32, index: UInt16, count: UInt16, epoch: UInt32, payload: Data) {
        // Primary restart signal: the host's stream epoch increases monotonically per pipeline
        // run. A HIGHER epoch means a brand-new run — drop all old state instantly so a fast
        // restart (within the seq-gap window) can't stall the stream. A LOWER epoch is a stale or
        // reordered fragment from the previous run; drop it (never roll currentEpoch backward, or
        // late old-run packets would ping-pong the reset against the new run). Older hosts send a
        // constant 0 here, so this stays a no-op for them and the seq-gap fallback takes over.
        if let current = currentEpoch {
            if epoch > current {
                onDiagnostic?("Stream epoch changed (\(current) -> \(epoch)) — resetting reassembly")
                reset()
                currentEpoch = epoch
            } else if epoch < current {
                return
            }
        } else {
            currentEpoch = epoch
        }

        if latestCompletedSeq > 0 {
            if seq == latestCompletedSeq {
                // Duplicate fragment for the frame we just completed — ignore.
                return
            }
            if seq < latestCompletedSeq {
                if latestCompletedSeq - seq > Self.streamRestartGap {
                    // Host restarted its stream (seq reset toward 0). Drop the old stream's
                    // state and accept this fragment as the start of the new stream.
                    onDiagnostic?("Stream restart detected (seq \(latestCompletedSeq) -> \(seq)) — resetting reassembly")
                    reset()
                } else {
                    // Genuinely stale/late fragment from the current stream.
                    return
                }
            }
        }

        guard count > 0 else {
            onDiagnostic?("Dropped fragment for seq=\(seq) with zero fragment count")
            return
        }
        guard index < count else {
            onDiagnostic?("Dropped fragment for seq=\(seq) with out-of-range index \(index)/\(count)")
            return
        }

        // Get or create pending frame
        if pending[seq] == nil {
            pending[seq] = PendingFrame(
                fragmentCount: count,
                fragments: [:],
                createdAt: mach_absolute_time()
            )
        } else if pending[seq]?.fragmentCount != count {
            onDiagnostic?("Reset reassembly for seq=\(seq) because fragment count changed from \(pending[seq]!.fragmentCount) to \(count)")
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
                } else {
                    onDiagnostic?("Reassembly gap for seq=\(seq) at fragment \(i)")
                    pending.removeValue(forKey: seq)
                    return
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
        currentEpoch = nil
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
