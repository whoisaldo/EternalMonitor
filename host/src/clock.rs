//! The host's stable monotonic clock: microseconds since PROCESS start.
//!
//! v1 stamped media with a per-pipeline-run epoch, which reset on every
//! restart and made receiver-side latency math meaningless. Everything in
//! protocol v2 (media capture timestamps, heartbeats, pong) uses this one
//! process-wide clock so timestamps survive pipeline restarts.

use std::time::Instant;

use once_cell::sync::Lazy;

static HOST_EPOCH: Lazy<Instant> = Lazy::new(Instant::now);

/// Microseconds since process start.
pub fn host_now_us() -> u64 {
    HOST_EPOCH.elapsed().as_micros() as u64
}

/// Convert an `Instant` captured elsewhere in the process to this clock.
/// Saturates to 0 for instants before process start (impossible in practice).
pub fn instant_to_us(instant: Instant) -> u64 {
    instant.duration_since(*HOST_EPOCH).as_micros() as u64
}

/// Force epoch initialization at startup so the first frame doesn't pay for it.
pub fn init() {
    Lazy::force(&HOST_EPOCH);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic_and_consistent() {
        init();
        let a = host_now_us();
        let instant = Instant::now();
        let b = instant_to_us(instant);
        let c = host_now_us();
        assert!(a <= b, "{a} <= {b}");
        assert!(b <= c, "{b} <= {c}");
    }
}
