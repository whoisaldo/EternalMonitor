//! Fragment reassembly that mirrors the iPad's `FrameAssembler.swift`
//! semantics exactly — epoch precedence, duplicate/stale rejection, the
//! seq-backward-jump restart heuristic, completion-time eviction of older
//! partial frames, and 100 ms stale-frame cleanup.
//!
//! The host never reassembles in production; this exists so the fake receiver
//! in the end-to-end tests (and any future host-side receiver) exercises the
//! same rules the real client applies, and so those rules are unit-testable
//! on every platform. If behavior here diverges from the Swift assembler,
//! the Swift side is the specification.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A backward jump in sequence numbers larger than this means the sender
/// restarted its pipeline and reset seq toward 0 (legacy hosts only — epoch
/// makes this explicit on current hosts).
pub const STREAM_RESTART_GAP: u32 = 256;

/// Partial frames older than this are evicted during periodic cleanup.
pub const STALE_FRAME_TIMEOUT: Duration = Duration::from_millis(100);

/// After this many CONSECUTIVE stale-epoch drops, the current epoch is treated
/// as bogus and re-synced to whatever is actually arriving.
///
/// Without this, one corrupted or spoofed fragment carrying a high epoch
/// latches an epoch the sender will never reach, and every real fragment is
/// dropped forever — video freezes while the control channel stays healthy, so
/// nothing notices. Genuine stragglers from a previous run can't trip it: the
/// new run's fragments interleave and are accepted, which resets the streak.
pub const EPOCH_RESYNC_THRESHOLD: u32 = 512;

/// Cleanup runs every this-many fragments.
const CLEANUP_INTERVAL: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Fragment carries an epoch lower than the current stream's.
    StaleEpoch,
    /// Duplicate fragment of the frame that just completed.
    DuplicateCompleted,
    /// Stale/late fragment of an already-superseded frame.
    StaleSeq,
    /// fragment_count == 0.
    ZeroCount,
    /// fragment_index >= fragment_count.
    IndexOutOfRange,
    /// Fragment count disagrees with the live frame's; first-seen count wins.
    CountMismatch,
    /// This index of this frame was already stored.
    DuplicateFragment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    /// Fragment stored; frame not yet complete.
    Stored,
    /// This fragment completed the frame; here is the reassembled payload.
    Completed(Vec<u8>),
    /// Fragment rejected.
    Dropped(DropReason),
}

#[derive(Debug)]
struct PendingFrame {
    fragment_count: u16,
    fragments: HashMap<u16, Vec<u8>>,
    created_at: Instant,
}

impl PendingFrame {
    /// Fragments that never arrived. THIS is what loss means: counting the
    /// ones that did arrive (as this mirror used to) reported a frame missing
    /// one fragment out of ten as nine lost, so the host ABR saw roughly nine
    /// times the real loss and stepped down on a healthy link.
    fn missing_fragments(&self) -> u64 {
        u64::from(self.fragment_count).saturating_sub(self.fragments.len() as u64)
    }
}

/// Cumulative counters for loss accounting (feeds receiver reports in v2).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblyCounters {
    pub frames_complete: u64,
    /// Partial frames discarded (superseded by a newer completion or evicted
    /// as stale).
    pub frames_dropped: u64,
    pub frags_received: u64,
    /// Fragments belonging to dropped partial frames.
    pub frags_lost: u64,
}

#[derive(Debug, Default)]
pub struct Reassembler {
    pending: HashMap<u32, PendingFrame>,
    latest_completed_seq: u32,
    cleanup_counter: u32,
    current_epoch: Option<u32>,
    /// Consecutive stale-epoch drops; see [`EPOCH_RESYNC_THRESHOLD`].
    stale_epoch_streak: u32,
    counters: ReassemblyCounters,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn counters(&self) -> ReassemblyCounters {
        self.counters
    }

    /// Number of in-flight partial frames.
    pub fn pending_frames(&self) -> usize {
        self.pending.len()
    }

    pub fn latest_completed_seq(&self) -> u32 {
        self.latest_completed_seq
    }

    /// Feed one fragment. `now` is injected so tests control time.
    pub fn add_fragment(
        &mut self,
        seq: u32,
        index: u16,
        count: u16,
        epoch: u32,
        payload: &[u8],
        now: Instant,
    ) -> AddOutcome {
        // Primary restart signal: a HIGHER epoch means a brand-new pipeline
        // run — drop all old state instantly. A LOWER epoch is a stale
        // fragment from the previous run; never roll the epoch backward.
        // Legacy hosts send 0, which behaves like any constant epoch and
        // leaves restart detection to the seq-gap fallback below.
        match self.current_epoch {
            Some(current) if epoch > current => {
                self.reset_internal();
                self.current_epoch = Some(epoch);
                self.stale_epoch_streak = 0;
            }
            Some(current) if epoch < current => {
                self.stale_epoch_streak += 1;
                if self.stale_epoch_streak < EPOCH_RESYNC_THRESHOLD {
                    return AddOutcome::Dropped(DropReason::StaleEpoch);
                }
                // Nothing has been accepted for a long run of drops, so the
                // epoch we are holding can't be the live one. Re-sync to the
                // stream that is actually arriving.
                self.reset_internal();
                self.current_epoch = Some(epoch);
                self.stale_epoch_streak = 0;
            }
            Some(_) => self.stale_epoch_streak = 0,
            None => {
                self.current_epoch = Some(epoch);
                self.stale_epoch_streak = 0;
            }
        }

        if self.latest_completed_seq > 0 {
            if seq == self.latest_completed_seq {
                return AddOutcome::Dropped(DropReason::DuplicateCompleted);
            }
            if seq < self.latest_completed_seq {
                if self.latest_completed_seq - seq > STREAM_RESTART_GAP {
                    // Sender restarted (seq reset toward 0): accept this
                    // fragment as the start of the new stream.
                    let epoch = self.current_epoch;
                    self.reset_internal();
                    self.current_epoch = epoch;
                } else {
                    return AddOutcome::Dropped(DropReason::StaleSeq);
                }
            }
        }

        if count == 0 {
            return AddOutcome::Dropped(DropReason::ZeroCount);
        }
        if index >= count {
            return AddOutcome::Dropped(DropReason::IndexOutOfRange);
        }

        match self.pending.get(&seq) {
            // Conflicting metadata for a live frame: the FIRST-seen count wins.
            // Rebuilding the frame here (as this mirror used to) let one late
            // duplicate or spoofed datagram throw away every fragment already
            // buffered for it.
            Some(frame) if frame.fragment_count != count => {
                return AddOutcome::Dropped(DropReason::CountMismatch);
            }
            Some(_) => {}
            None => {
                self.pending.insert(
                    seq,
                    PendingFrame {
                        fragment_count: count,
                        fragments: HashMap::new(),
                        created_at: now,
                    },
                );
            }
        }

        let frame = self
            .pending
            .get_mut(&seq)
            .expect("pending frame present or just inserted");
        // First write wins, and only a first write counts as received: a
        // replayed fragment must not overwrite good bytes or inflate the
        // receive count the loss math is derived from.
        if frame.fragments.contains_key(&index) {
            return AddOutcome::Dropped(DropReason::DuplicateFragment);
        }
        frame.fragments.insert(index, payload.to_vec());
        self.counters.frags_received += 1;
        let frame = self
            .pending
            .get_mut(&seq)
            .expect("pending frame present or just inserted");

        let mut outcome = AddOutcome::Stored;
        if frame.fragments.len() == usize::from(frame.fragment_count) {
            let mut assembled = Vec::with_capacity(frame.fragments.values().map(Vec::len).sum());
            for i in 0..frame.fragment_count {
                let fragment = frame
                    .fragments
                    .get(&i)
                    .expect("complete frame has every index");
                assembled.extend_from_slice(fragment);
            }

            self.latest_completed_seq = seq;
            self.pending.remove(&seq);
            self.counters.frames_complete += 1;

            // Evict every older partial frame: delivering it after a newer
            // frame would corrupt decode order.
            let mut dropped_frames = 0u64;
            let mut dropped_frags = 0u64;
            self.pending.retain(|&pending_seq, frame| {
                let keep = pending_seq > seq;
                if !keep {
                    dropped_frames += 1;
                    dropped_frags += frame.missing_fragments();
                }
                keep
            });
            self.counters.frames_dropped += dropped_frames;
            self.counters.frags_lost += dropped_frags;

            outcome = AddOutcome::Completed(assembled);
        }

        self.cleanup_counter += 1;
        if self.cleanup_counter.is_multiple_of(CLEANUP_INTERVAL) {
            self.evict_stale(now);
        }

        outcome
    }

    /// Drops all reassembly state (new session / manual disconnect).
    pub fn reset(&mut self) {
        self.reset_internal();
        self.current_epoch = None;
        self.stale_epoch_streak = 0;
    }

    fn reset_internal(&mut self) {
        for (_, frame) in self.pending.drain() {
            self.counters.frames_dropped += 1;
            self.counters.frags_lost += frame.missing_fragments();
        }
        self.latest_completed_seq = 0;
        self.cleanup_counter = 0;
    }

    fn evict_stale(&mut self, now: Instant) {
        let counters = &mut self.counters;
        self.pending.retain(|_, frame| {
            let fresh = now.duration_since(frame.created_at) < STALE_FRAME_TIMEOUT;
            if !fresh {
                counters.frames_dropped += 1;
                counters.frags_lost += frame.missing_fragments();
            }
            fresh
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(
        r: &mut Reassembler,
        seq: u32,
        index: u16,
        count: u16,
        epoch: u32,
        byte: u8,
        now: Instant,
    ) -> AddOutcome {
        r.add_fragment(seq, index, count, epoch, &[byte], now)
    }

    #[test]
    fn assembles_in_order_and_out_of_order() {
        let now = Instant::now();
        let mut r = Reassembler::new();

        assert_eq!(feed(&mut r, 1, 0, 3, 1, 0xA, now), AddOutcome::Stored);
        assert_eq!(feed(&mut r, 1, 2, 3, 1, 0xC, now), AddOutcome::Stored);
        assert_eq!(
            feed(&mut r, 1, 1, 3, 1, 0xB, now),
            AddOutcome::Completed(vec![0xA, 0xB, 0xC])
        );
        assert_eq!(r.counters().frames_complete, 1);

        // Reordered single-fragment frame completes immediately.
        assert_eq!(
            feed(&mut r, 2, 0, 1, 1, 0xD, now),
            AddOutcome::Completed(vec![0xD])
        );
    }

    #[test]
    fn duplicate_of_completed_frame_is_ignored() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 1, 1, 0xA, now);
        assert_eq!(
            feed(&mut r, 1, 0, 1, 1, 0xA, now),
            AddOutcome::Dropped(DropReason::DuplicateCompleted)
        );
    }

    #[test]
    fn stale_seq_within_gap_is_dropped_but_big_backjump_resets() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 500, 0, 1, 1, 0xA, now);

        assert_eq!(
            feed(&mut r, 499, 0, 1, 1, 0xB, now),
            AddOutcome::Dropped(DropReason::StaleSeq)
        );

        // Backward jump beyond the gap = legacy restart detection.
        assert_eq!(
            feed(&mut r, 1, 0, 1, 1, 0xC, now),
            AddOutcome::Completed(vec![0xC])
        );
        assert_eq!(r.latest_completed_seq(), 1);
    }

    #[test]
    fn higher_epoch_resets_lower_epoch_is_dropped() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 900, 0, 1, 5, 0xA, now);

        // Same-epoch stale fragment: dropped by seq rules.
        assert_eq!(
            feed(&mut r, 890, 0, 1, 5, 0xB, now),
            AddOutcome::Dropped(DropReason::StaleSeq)
        );

        // New pipeline run: epoch bumps, small seq accepted immediately.
        assert_eq!(
            feed(&mut r, 1, 0, 1, 6, 0xC, now),
            AddOutcome::Completed(vec![0xC])
        );

        // Straggler from the old run: dropped on epoch alone.
        assert_eq!(
            feed(&mut r, 901, 0, 1, 5, 0xD, now),
            AddOutcome::Dropped(DropReason::StaleEpoch)
        );
    }

    #[test]
    fn one_bogus_epoch_cannot_strand_the_stream() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 10, 0, 1, 5, 0xA, now);

        // One corrupted/spoofed fragment claiming the maximum epoch.
        feed(&mut r, 11, 0, 1, u32::MAX, 0xFF, now);

        // The real stream is now "stale" against an epoch it can never reach.
        for i in 0..(EPOCH_RESYNC_THRESHOLD - 1) {
            assert_eq!(
                feed(&mut r, 100 + i, 0, 1, 5, 0xB, now),
                AddOutcome::Dropped(DropReason::StaleEpoch),
                "fragment {i} should still be dropped before the resync threshold"
            );
        }

        // Crossing the threshold re-syncs to the stream that is really there.
        assert_eq!(
            feed(&mut r, 900, 0, 1, 5, 0xC, now),
            AddOutcome::Completed(vec![0xC]),
            "the receiver must recover instead of staying bricked forever"
        );
        // And it keeps flowing afterwards.
        assert_eq!(
            feed(&mut r, 901, 0, 1, 5, 0xD, now),
            AddOutcome::Completed(vec![0xD])
        );
    }

    #[test]
    fn interleaved_stragglers_never_trip_the_resync() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 1, 7, 0xA, now);

        // A real restart: epoch 8 is live, epoch 7 stragglers keep arriving
        // alongside it. Far more stale drops than the threshold, but the
        // accepted fragments in between must keep resetting the streak.
        for i in 0..(EPOCH_RESYNC_THRESHOLD * 2) {
            assert_eq!(
                feed(&mut r, 1000 + i, 0, 1, 8, 0xB, now),
                AddOutcome::Completed(vec![0xB])
            );
            assert_eq!(
                feed(&mut r, 500 + i, 0, 1, 7, 0xC, now),
                AddOutcome::Dropped(DropReason::StaleEpoch),
                "old-run straggler {i} must stay dropped"
            );
        }
    }

    #[test]
    fn completion_evicts_older_partials_and_counts_them_dropped() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        // Frame 1 partial (1 of 2 fragments), then frame 2 completes.
        feed(&mut r, 1, 0, 2, 1, 0xA, now);
        assert_eq!(
            feed(&mut r, 2, 0, 1, 1, 0xB, now),
            AddOutcome::Completed(vec![0xB])
        );
        assert_eq!(r.pending_frames(), 0);
        assert_eq!(r.counters().frames_dropped, 1);
        // One fragment of the two-fragment frame never arrived.
        assert_eq!(r.counters().frags_lost, 1);

        // The evicted frame's late fragment is now stale.
        assert_eq!(
            feed(&mut r, 1, 1, 2, 1, 0xC, now),
            AddOutcome::Dropped(DropReason::StaleSeq)
        );
    }

    #[test]
    fn first_seen_fragment_count_wins_and_progress_survives() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 3, 1, 0xA, now);

        // A fragment claiming a different count for a live frame is ignored;
        // it must not discard what has already been buffered.
        assert_eq!(
            feed(&mut r, 1, 1, 2, 1, 0xB, now),
            AddOutcome::Dropped(DropReason::CountMismatch)
        );

        assert_eq!(feed(&mut r, 1, 1, 3, 1, 0xB, now), AddOutcome::Stored);
        assert_eq!(
            feed(&mut r, 1, 2, 3, 1, 0xC, now),
            AddOutcome::Completed(vec![0xA, 0xB, 0xC]),
            "the original fragments must still be there"
        );
    }

    #[test]
    fn replayed_fragment_neither_overwrites_nor_counts_twice() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 2, 1, 0xA, now);
        assert_eq!(
            feed(&mut r, 1, 0, 2, 1, 0xFF, now),
            AddOutcome::Dropped(DropReason::DuplicateFragment)
        );
        assert_eq!(r.counters().frags_received, 1, "a replay is not a receipt");
        assert_eq!(
            feed(&mut r, 1, 1, 2, 1, 0xB, now),
            AddOutcome::Completed(vec![0xA, 0xB]),
            "the first write must win"
        );
    }

    #[test]
    fn loss_counts_the_fragments_that_never_arrived() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        // Frame 1 gets 9 of 10 fragments, then a newer frame completes and
        // evicts it: exactly ONE fragment was lost, not nine.
        for index in 0..9u16 {
            feed(&mut r, 1, index, 10, 1, 0xA, now);
        }
        assert_eq!(
            feed(&mut r, 2, 0, 1, 1, 0xB, now),
            AddOutcome::Completed(vec![0xB])
        );
        assert_eq!(r.counters().frames_dropped, 1);
        assert_eq!(r.counters().frags_lost, 1);
    }

    #[test]
    fn invalid_fragments_are_rejected() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        assert_eq!(
            feed(&mut r, 1, 0, 0, 1, 0xA, now),
            AddOutcome::Dropped(DropReason::ZeroCount)
        );
        assert_eq!(
            feed(&mut r, 1, 2, 2, 1, 0xA, now),
            AddOutcome::Dropped(DropReason::IndexOutOfRange)
        );
    }

    #[test]
    fn stale_partials_are_evicted_on_cleanup_tick() {
        let start = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 2, 1, 0xA, start);

        // 99 more fragments to hit the cleanup interval, well past the timeout.
        let later = start + Duration::from_millis(500);
        for i in 0..99u32 {
            feed(&mut r, 10 + i, 0, 2, 1, 0xB, later);
        }
        assert!(
            !r.pending.contains_key(&1),
            "stale frame must be evicted by the periodic cleanup"
        );
        assert!(r.counters().frames_dropped >= 1);
    }

    #[test]
    fn reset_clears_epoch_and_state() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 5, 0, 2, 9, 0xA, now);
        r.reset();
        assert_eq!(r.pending_frames(), 0);
        // After reset any epoch is accepted again.
        assert_eq!(
            feed(&mut r, 1, 0, 1, 2, 0xB, now),
            AddOutcome::Completed(vec![0xB])
        );
    }
}
