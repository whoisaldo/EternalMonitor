//! Send pacing for large frames.
//!
//! A keyframe fragments into hundreds of datagrams; blasting them back-to-back
//! overflows WiFi access-point queues exactly on the frames that matter most.
//! The pacer spreads a frame's datagrams into batches with short gaps — under
//! a HARD wall-clock budget, so a stretched sleep (macOS timer coalescing,
//! Windows 15.6 ms granularity) can only ever degrade to "no pacing", never to
//! added latency.

use std::time::{Duration, Instant};

/// Frames at or below this many fragments go out unpaced.
pub const PACE_THRESHOLD_FRAGS: usize = 16;
/// Datagrams per burst between gaps.
pub const BATCH_SIZE: usize = 32;
/// The whole frame must be on the wire within this budget.
pub const MAX_SPREAD: Duration = Duration::from_millis(3);
/// Nominal gap between batches (recomputed against the hard budget).
pub const BATCH_GAP: Duration = Duration::from_micros(250);

/// Tracks the pacing schedule for one frame's send loop.
pub struct FramePacer {
    started: Instant,
    total_frags: usize,
    sent: usize,
}

impl FramePacer {
    pub fn new(total_frags: usize, now: Instant) -> Self {
        Self {
            started: now,
            total_frags,
            sent: 0,
        }
    }

    /// Record one datagram sent; returns how long to pause before the next
    /// one (zero for most datagrams — only batch boundaries pause, and only
    /// while the budget allows).
    pub fn after_send(&mut self, now: Instant) -> Duration {
        self.sent += 1;
        if self.total_frags <= PACE_THRESHOLD_FRAGS || self.sent >= self.total_frags {
            return Duration::ZERO;
        }
        if !self.sent.is_multiple_of(BATCH_SIZE) {
            return Duration::ZERO;
        }
        // Hard budget: whatever the OS did to previous gaps, never let pacing
        // push the frame past MAX_SPREAD total.
        let elapsed = now.duration_since(self.started);
        if elapsed + BATCH_GAP > MAX_SPREAD {
            return Duration::ZERO;
        }
        BATCH_GAP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_frames_are_never_paced() {
        let now = Instant::now();
        let mut pacer = FramePacer::new(PACE_THRESHOLD_FRAGS, now);
        for _ in 0..PACE_THRESHOLD_FRAGS {
            assert_eq!(pacer.after_send(now), Duration::ZERO);
        }
    }

    #[test]
    fn large_frames_pause_at_batch_boundaries() {
        let now = Instant::now();
        let mut pacer = FramePacer::new(100, now);
        let mut pauses = 0;
        for i in 1..=100 {
            let pause = pacer.after_send(now);
            if pause > Duration::ZERO {
                pauses += 1;
                assert!(
                    i % BATCH_SIZE == 0,
                    "pause must land on a batch boundary, got {i}"
                );
            }
        }
        // 100 frags -> boundaries at 32, 64, 96 (not after the last datagram).
        assert_eq!(pauses, 3);
    }

    #[test]
    fn budget_exhaustion_disables_further_gaps() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(1000, start);
        // Pretend the first gap was stretched way past the budget.
        let late = start + Duration::from_millis(10);
        for i in 1..=BATCH_SIZE * 3 {
            let pause = pacer.after_send(late);
            assert_eq!(
                pause,
                Duration::ZERO,
                "no further pacing once the budget is spent (datagram {i})"
            );
        }
    }

    #[test]
    fn never_pauses_after_the_final_datagram() {
        let now = Instant::now();
        let mut pacer = FramePacer::new(BATCH_SIZE, now);
        for i in 1..=BATCH_SIZE {
            let pause = pacer.after_send(now);
            if i == BATCH_SIZE {
                assert_eq!(pause, Duration::ZERO);
            }
        }
    }
}
