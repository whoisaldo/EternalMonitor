import Foundation
import os

/// Reassembles fragmented UDP datagrams into complete FlatBuffer payloads.
/// Called exclusively from the UDP receiver's serial queue — no locking needed.
///
/// Hardened against malformed/hostile input: fragment counts, in-flight frame
/// counts, and total buffered bytes are all capped, so no packet sequence can
/// balloon memory. (`eternal-wire`'s `reassembly.rs` mirrors these semantics
/// for the host-side tests; this file is the specification.)
final class FrameAssembler {
    /// Completed access unit: raw Annex B payload + the frame metadata every
    /// fragment carried (protocol v2 repeats it per datagram).
    var onFrameAssembled: ((Data, _ seq: UInt32, _ captureTimestampUs: UInt64, _ isKeyframe: Bool) -> Void)?
    var onDiagnostic: ((String) -> Void)?

    /// A frame may span at most this many fragments (~1.4 MB at 1384-byte
    /// payloads). The u16 field allows 65535 (≈90 MB) — a memory bomb, not a
    /// video frame.
    static let maxFragmentCount: UInt16 = 1024
    /// At most this many partial frames in flight; the oldest is dropped first.
    static let maxPendingFrames = 8
    /// Hard ceiling on buffered fragment bytes across all partial frames.
    static let maxPendingBytes = 8 * 1024 * 1024

    /// Cumulative per-connection loss accounting, safe to read from any
    /// thread (feeds receiver reports and the HUD).
    struct Counters {
        var framesComplete: UInt64 = 0
        var framesDropped: UInt64 = 0
        var fragsReceived: UInt64 = 0
        var fragsLost: UInt64 = 0
    }

    let counters = OSAllocatedUnfairLock<Counters>(initialState: Counters())

    private var pending: [UInt32: PendingFrame] = [:]
    private var pendingBytes = 0
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

    /// Consecutive stale-epoch drops, and the count after which the epoch we are
    /// holding is treated as bogus and re-synced to whatever is arriving.
    ///
    /// One corrupted or spoofed fragment carrying a high epoch would otherwise
    /// latch an epoch the host will never reach, and every real fragment would
    /// be dropped for the rest of the session. Nothing would notice: control
    /// heartbeats keep flowing, so the liveness watchdog stays happy while the
    /// video is frozen. Genuine stragglers from a previous run can't trip this,
    /// because the new run's fragments interleave and reset the streak.
    private var staleEpochStreak: UInt32 = 0
    private static let epochResyncThreshold: UInt32 = 512

    struct PendingFrame {
        let fragmentCount: UInt16
        let isKeyframe: Bool
        let captureTimestampUs: UInt64
        var fragments: [UInt16: Data]
        var byteCount: Int
        let createdAt: UInt64  // mach_absolute_time

        var isComplete: Bool {
            fragments.count == Int(fragmentCount)
        }
    }

    func addFragment(
        seq: UInt32,
        index: UInt16,
        count: UInt16,
        epoch: UInt32,
        isKeyframe: Bool,
        captureTimestampUs: UInt64,
        payload: Data
    ) {
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
                staleEpochStreak += 1
                guard staleEpochStreak >= Self.epochResyncThreshold else { return }
                // Nothing has been accepted across a long run of drops, so the
                // epoch we are holding can't be the live one. Re-sync to the
                // stream that is actually arriving rather than stay frozen.
                onDiagnostic?(
                    "Epoch \(current) never resumed after \(staleEpochStreak) dropped fragments"
                        + " — re-syncing to epoch \(epoch)"
                )
                reset()
                currentEpoch = epoch
            } else {
                staleEpochStreak = 0
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
                    let epoch = currentEpoch
                    reset()
                    currentEpoch = epoch
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
        guard count <= Self.maxFragmentCount else {
            onDiagnostic?("Dropped fragment for seq=\(seq): fragment count \(count) exceeds cap \(Self.maxFragmentCount)")
            return
        }
        guard index < count else {
            onDiagnostic?("Dropped fragment for seq=\(seq) with out-of-range index \(index)/\(count)")
            return
        }

        // Get or create pending frame
        if let existing = pending[seq] {
            if existing.fragmentCount != count {
                // Conflicting metadata for a live frame. The first-seen count wins —
                // a late duplicate must not wipe accumulated progress.
                onDiagnostic?("Ignored fragment for seq=\(seq) with mismatched count \(count) (frame has \(existing.fragmentCount))")
                return
            }
        } else {
            enforceCapacityForNewFrame(incoming: seq)
            pending[seq] = PendingFrame(
                fragmentCount: count,
                isKeyframe: isKeyframe,
                captureTimestampUs: captureTimestampUs,
                fragments: [:],
                byteCount: 0,
                createdAt: mach_absolute_time()
            )
        }

        if pending[seq]?.fragments[index] == nil {
            pending[seq]?.fragments[index] = payload
            pending[seq]?.byteCount += payload.count
            pendingBytes += payload.count
            counters.withLock { $0.fragsReceived += 1 }
        }

        // Check if frame is complete
        if let frame = pending[seq], frame.isComplete {
            // Reassemble in fragment index order
            var assembled = Data(capacity: frame.byteCount)
            for i in 0..<frame.fragmentCount {
                if let fragment = frame.fragments[i] {
                    assembled.append(fragment)
                } else {
                    onDiagnostic?("Reassembly gap for seq=\(seq) at fragment \(i)")
                    removePending(seq)
                    return
                }
            }

            latestCompletedSeq = seq
            removePending(seq)
            counters.withLock { $0.framesComplete += 1 }

            // Evict any frames older than the completed one — delivering them
            // after a newer frame would corrupt decode order.
            for staleSeq in pending.keys where staleSeq <= seq {
                dropPending(staleSeq)
            }

            onFrameAssembled?(assembled, seq, frame.captureTimestampUs, frame.isKeyframe)
            evictStale()
            return
        }

        // Periodic cleanup of stale entries
        cleanupCounter += 1
        if cleanupCounter % 32 == 0 {
            evictStale()
        }
    }

    func reset() {
        pending.removeAll()
        pendingBytes = 0
        latestCompletedSeq = 0
        cleanupCounter = 0
        currentEpoch = nil
        staleEpochStreak = 0
        counters.withLock { $0 = Counters() }
    }

    private func removePending(_ seq: UInt32) {
        if let removed = pending.removeValue(forKey: seq) {
            pendingBytes -= removed.byteCount
        }
    }

    /// Remove a partial frame that will never complete, counting its loss.
    private func dropPending(_ seq: UInt32) {
        if let removed = pending.removeValue(forKey: seq) {
            pendingBytes -= removed.byteCount
            counters.withLock {
                $0.framesDropped += 1
                $0.fragsLost += UInt64(max(Int(removed.fragmentCount) - removed.fragments.count, 0))
            }
        }
    }

    /// Make room before inserting a new partial frame: never exceed the frame
    /// or byte caps. Drops the OLDEST pending frames first (they are the least
    /// likely to ever complete).
    private func enforceCapacityForNewFrame(incoming: UInt32) {
        while pending.count >= Self.maxPendingFrames || pendingBytes >= Self.maxPendingBytes {
            guard let oldest = pending.keys.min() else { break }
            onDiagnostic?("Dropped partial frame seq=\(oldest) to admit seq=\(incoming) (capacity)")
            dropPending(oldest)
        }
    }

    private func evictStale() {
        let now = mach_absolute_time()
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        let numer = UInt64(info.numer)
        let denom = UInt64(info.denom)

        for (seq, frame) in pending {
            let elapsedNs = ((now - frame.createdAt) * numer) / denom
            if elapsedNs >= 100_000_000 {  // 100ms timeout
                dropPending(seq)
            }
        }
    }
}
