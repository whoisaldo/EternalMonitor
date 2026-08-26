//! Adaptive bitrate controller: a pure rung-ladder policy evaluated on each
//! receiver report. The transport applies the returned target through the
//! encoder-reconfigure path; this module never touches sockets or encoders.

use std::time::{Duration, Instant};

use eternal_wire::v2::control::ReceiverReport;
use tracing::info;

/// Candidate bitrates, low to high. The active ladder is this intersected
/// with the user's ceiling (the GUI "Max bitrate" slider).
const LADDER_BPS: [u32; 7] = [
    4_000_000, 6_000_000, 8_000_000, 10_000_000, 12_000_000, 15_000_000, 20_000_000,
];

const DOWN_COOLDOWN: Duration = Duration::from_secs(3);
const UP_COOLDOWN: Duration = Duration::from_secs(15);
/// This long continuously clean before stepping up.
const CLEAN_WINDOW: Duration = Duration::from_secs(15);
/// Freeze the controller when reports stop (liveness machinery owns that).
const REPORT_STALE_AFTER: Duration = Duration::from_secs(2);

/// Fragment-loss thresholds per report interval.
const LOSS_DOWN_ONE: f64 = 0.02;
const LOSS_DOWN_TWO: f64 = 0.10;
const LOSS_CLEAN: f64 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbrDecision {
    pub target_bps: u32,
    pub changed: bool,
}

pub struct AbrController {
    enabled: bool,
    ceiling_bps: u32,
    current_bps: u32,
    last_change: Option<(Instant, bool)>, // (when, was_downgrade)
    clean_since: Option<Instant>,
    last_report_at: Option<Instant>,
    prev_cumulative: Option<(u32, u32)>, // (frags_received, frags_lost)
}

impl AbrController {
    /// Starts at min(ceiling, 15 Mbps) per the plan.
    pub fn new(ceiling_bps: u32, enabled: bool) -> Self {
        let start = clamp_to_ladder(ceiling_bps.min(15_000_000), ceiling_bps);
        Self {
            enabled,
            ceiling_bps,
            current_bps: start,
            last_change: None,
            clean_since: None,
            last_report_at: None,
            prev_cumulative: None,
        }
    }

    pub fn current_bps(&self) -> u32 {
        self.current_bps
    }

    /// The user moved the ceiling: re-clamp immediately.
    pub fn set_ceiling(&mut self, ceiling_bps: u32) -> AbrDecision {
        self.ceiling_bps = ceiling_bps;
        let clamped = clamp_to_ladder(self.current_bps, ceiling_bps);
        let changed = clamped != self.current_bps;
        self.current_bps = clamped;
        AbrDecision {
            target_bps: self.current_bps,
            changed,
        }
    }

    /// Evaluate one receiver report (cumulative counters — this diffs them).
    pub fn on_report(&mut self, report: &ReceiverReport, now: Instant) -> AbrDecision {
        let unchanged = AbrDecision {
            target_bps: self.current_bps,
            changed: false,
        };
        if !self.enabled {
            return unchanged;
        }

        // Interval deltas from cumulative counters.
        let (delta_received, delta_lost) = match self.prev_cumulative {
            Some((prev_received, prev_lost)) => (
                report.frags_received.saturating_sub(prev_received),
                report.frags_lost.saturating_sub(prev_lost),
            ),
            None => (report.frags_received, report.frags_lost),
        };
        self.prev_cumulative = Some((report.frags_received, report.frags_lost));

        // A stale gap between reports invalidates the clean streak.
        if let Some(last) = self.last_report_at {
            if now.duration_since(last) > REPORT_STALE_AFTER {
                self.clean_since = None;
            }
        }
        self.last_report_at = Some(now);

        let total = delta_received + delta_lost;
        let loss = if total == 0 {
            0.0
        } else {
            f64::from(delta_lost) / f64::from(total)
        };
        let congested_queue = report.assembler_depth >= 3;

        // ---- Downgrade path ----
        let down_rungs = if loss > LOSS_DOWN_TWO {
            2
        } else if loss > LOSS_DOWN_ONE || congested_queue {
            1
        } else {
            0
        };
        if down_rungs > 0 {
            self.clean_since = None;
            let in_cooldown = matches!(
                self.last_change,
                Some((when, true)) if now.duration_since(when) < DOWN_COOLDOWN
            );
            if !in_cooldown {
                let target = step(self.current_bps, -(down_rungs), self.ceiling_bps);
                if target != self.current_bps {
                    info!(
                        from = self.current_bps,
                        to = target,
                        loss_pct = format!("{:.1}", loss * 100.0),
                        "ABR stepping bitrate down"
                    );
                    self.current_bps = target;
                    self.last_change = Some((now, true));
                    return AbrDecision {
                        target_bps: target,
                        changed: true,
                    };
                }
            }
            return unchanged;
        }

        // ---- Upgrade path ----
        if loss < LOSS_CLEAN && !congested_queue {
            let clean_since = *self.clean_since.get_or_insert(now);
            let up_cooldown_over = match self.last_change {
                Some((when, _)) => now.duration_since(when) >= UP_COOLDOWN,
                None => true,
            };
            if now.duration_since(clean_since) >= CLEAN_WINDOW && up_cooldown_over {
                let target = step(self.current_bps, 1, self.ceiling_bps);
                if target != self.current_bps {
                    info!(
                        from = self.current_bps,
                        to = target,
                        "ABR stepping bitrate up"
                    );
                    self.current_bps = target;
                    self.last_change = Some((now, false));
                    self.clean_since = Some(now);
                    return AbrDecision {
                        target_bps: target,
                        changed: true,
                    };
                }
            }
        } else {
            self.clean_since = None;
        }

        unchanged
    }
}

/// Nearest ladder rung at or below `bps`, capped by `ceiling`.
fn clamp_to_ladder(bps: u32, ceiling: u32) -> u32 {
    let cap = bps.min(ceiling);
    LADDER_BPS
        .iter()
        .rev()
        .copied()
        .find(|&rung| rung <= cap)
        .unwrap_or(LADDER_BPS[0])
}

/// Move `rungs` steps up (+) or down (−) the ladder from `from`, capped.
fn step(from: u32, rungs: i32, ceiling: u32) -> u32 {
    let current_index = LADDER_BPS
        .iter()
        .position(|&rung| rung >= from)
        .unwrap_or(LADDER_BPS.len() - 1);
    let target_index =
        (current_index as i32 + rungs).clamp(0, LADDER_BPS.len() as i32 - 1) as usize;
    LADDER_BPS[target_index].min(clamp_to_ladder(u32::MAX, ceiling))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(received: u32, lost: u32, depth: u8) -> ReceiverReport {
        ReceiverReport {
            frags_received: received,
            frags_lost: lost,
            assembler_depth: depth,
            ..Default::default()
        }
    }

    #[test]
    fn starts_at_min_of_ceiling_and_fifteen() {
        assert_eq!(
            AbrController::new(20_000_000, true).current_bps(),
            15_000_000
        );
        assert_eq!(AbrController::new(8_000_000, true).current_bps(), 8_000_000);
        assert_eq!(AbrController::new(5_000_000, true).current_bps(), 4_000_000);
    }

    #[test]
    fn heavy_loss_steps_down_two_rungs_with_cooldown() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        // 15% loss -> two rungs: 15M -> 10M.
        let d = abr.on_report(&report(850, 150, 0), t0);
        assert!(d.changed);
        assert_eq!(d.target_bps, 10_000_000);

        // Still lossy 1s later: inside the 3s down-cooldown, no change.
        let d = abr.on_report(&report(1700, 300, 0), t0 + Duration::from_secs(1));
        assert!(!d.changed);

        // After the cooldown, mild loss steps one more rung: 10M -> 8M.
        let d = abr.on_report(&report(2540, 340, 0), t0 + Duration::from_secs(4));
        assert!(d.changed);
        assert_eq!(d.target_bps, 8_000_000);
    }

    #[test]
    fn queue_depth_alone_steps_down() {
        let mut abr = AbrController::new(20_000_000, true);
        let d = abr.on_report(&report(1000, 0, 4), Instant::now());
        assert!(d.changed);
        assert_eq!(d.target_bps, 12_000_000);
    }

    #[test]
    fn clean_window_steps_up_after_cooldowns() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        // Step down first: 15M -> 12M.
        let d = abr.on_report(&report(950, 50, 0), t0);
        assert_eq!(d.target_bps, 12_000_000);

        // Clean reports every 500ms; must NOT step up before 15s clean.
        let mut cumulative = 1000;
        let mut last = AbrDecision {
            target_bps: 12_000_000,
            changed: false,
        };
        for i in 1..=40 {
            cumulative += 500;
            let at = t0 + Duration::from_millis(500 * i);
            last = abr.on_report(&report(cumulative, 50, 0), at);
            if last.changed {
                assert!(
                    at.duration_since(t0) >= Duration::from_secs(15),
                    "stepped up too early at {:?}",
                    at.duration_since(t0)
                );
                break;
            }
        }
        assert!(last.changed, "a sustained clean window must step up");
        assert_eq!(last.target_bps, 15_000_000);
    }

    #[test]
    fn report_gap_resets_clean_streak() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        abr.on_report(&report(950, 50, 0), t0); // down to 12M
                                                // 10s of clean...
        let mut cumulative = 1000;
        for i in 1..=20 {
            cumulative += 500;
            abr.on_report(
                &report(cumulative, 50, 0),
                t0 + Duration::from_millis(500 * i),
            );
        }
        // ...then a 3s report outage, then clean again: the streak restarts,
        // so no upgrade until ~15s after the outage.
        let resume = t0 + Duration::from_secs(13);
        for i in 0..=8 {
            cumulative += 500;
            let d = abr.on_report(&report(cumulative, 50, 0), resume + Duration::from_secs(i));
            assert!(!d.changed, "must not upgrade at +{i}s after an outage");
        }
    }

    #[test]
    fn ceiling_clamps_immediately_and_disabled_never_changes() {
        let mut abr = AbrController::new(20_000_000, true);
        let d = abr.set_ceiling(9_000_000);
        assert!(d.changed);
        assert_eq!(d.target_bps, 8_000_000);

        let mut off = AbrController::new(20_000_000, false);
        let d = off.on_report(&report(500, 500, 5), Instant::now());
        assert!(!d.changed, "disabled controller must never adapt");
    }

    #[test]
    fn floor_is_four_megabits() {
        let mut abr = AbrController::new(20_000_000, true);
        let mut now = Instant::now();
        let mut cumulative = (0u32, 0u32);
        for _ in 0..12 {
            cumulative = (cumulative.0 + 800, cumulative.1 + 200);
            abr.on_report(&report(cumulative.0, cumulative.1, 0), now);
            now += Duration::from_secs(4);
        }
        assert_eq!(abr.current_bps(), 4_000_000, "must bottom out at the floor");
    }
}
