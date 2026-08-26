//! Pipeline supervisor v2: owns the pipeline thread across restarts, watches
//! stage health, auto-restarts crashed/wedged generations with backoff, and
//! brakes restart storms into an explicit Failed state.
//!
//! The state machine is pure (injected clock, message-driven) so every rule
//! is unit-testable; the thin runtime loop at the bottom does the actual
//! spawning/joining.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tracing::{error, info, warn};

use crate::control::{SharedControl, SupervisorCommand};
use crate::gpu::GpuInfo;
use crate::stats::PIPELINE_STATS;

/// The current pipeline generation. Stage health reports carry the generation
/// they belong to; anything from an older generation is ignored (a "poisoned"
/// zombie thread can never confuse the supervisor about the live pipeline).
pub static CURRENT_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Capture,
    Encoder,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOutcome {
    /// Clean exit (stop requested, channel closed).
    Completed,
    /// The stage died with an error while the pipeline was supposed to run.
    Failed(String),
}

#[derive(Debug)]
pub enum SupMsg {
    Command(SupervisorCommand),
    Health {
        generation: u64,
        stage: Stage,
        outcome: StageOutcome,
    },
}

/// Auto-restart policy: exponential backoff, and a rolling-window storm brake.
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const STORM_WINDOW: Duration = Duration::from_secs(60);
const STORM_LIMIT: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorState {
    Running {
        generation: u64,
    },
    /// Waiting out a backoff before the next automatic restart.
    BackingOff {
        until: Instant,
        attempt: u32,
    },
    /// Too many automatic restarts in the window; a manual Start is required.
    Failed {
        reason: String,
    },
    Stopped,
    ShutDown,
}

/// What the runtime loop must do after an event.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Join the current pipeline (bounded) and spawn a fresh generation.
    Restart,
    /// Join the current pipeline; do not respawn.
    StopPipeline,
    /// Spawn a fresh generation (from Stopped/Failed).
    Start,
    /// Join and exit the supervisor loop.
    Shutdown,
}

/// Pure decision core.
pub struct Machine {
    pub state: SupervisorState,
    restarts: Vec<Instant>,
    attempt: u32,
}

impl Machine {
    pub fn new(initial_generation: u64) -> Self {
        Self {
            state: SupervisorState::Running {
                generation: initial_generation,
            },
            restarts: Vec::new(),
            attempt: 0,
        }
    }

    pub fn on_command(&mut self, command: &SupervisorCommand) -> Effect {
        match command {
            SupervisorCommand::Restart => match self.state {
                SupervisorState::ShutDown => Effect::None,
                _ => {
                    // User/receiver-driven restarts reset the failure budget.
                    self.attempt = 0;
                    self.restarts.clear();
                    Effect::Restart
                }
            },
            SupervisorCommand::Stop => match self.state {
                SupervisorState::Running { .. } | SupervisorState::BackingOff { .. } => {
                    self.state = SupervisorState::Stopped;
                    Effect::StopPipeline
                }
                _ => Effect::None,
            },
            SupervisorCommand::Start => match self.state {
                SupervisorState::Stopped | SupervisorState::Failed { .. } => {
                    self.attempt = 0;
                    self.restarts.clear();
                    Effect::Start
                }
                _ => Effect::None,
            },
            SupervisorCommand::Shutdown => {
                self.state = SupervisorState::ShutDown;
                Effect::Shutdown
            }
        }
    }

    /// A stage from the LIVE generation ended.
    pub fn on_stage_exit(
        &mut self,
        generation: u64,
        stage: Stage,
        outcome: &StageOutcome,
        now: Instant,
    ) -> Effect {
        let SupervisorState::Running {
            generation: live_generation,
        } = self.state
        else {
            return Effect::None;
        };
        if generation != live_generation {
            return Effect::None; // poisoned: an old generation's ghost
        }
        match outcome {
            StageOutcome::Completed => Effect::None,
            StageOutcome::Failed(reason) => self.auto_restart(stage, reason, now),
        }
    }

    /// A watchdog decided the live pipeline is wedged.
    pub fn on_wedged(&mut self, reason: &str, now: Instant) -> Effect {
        if !matches!(self.state, SupervisorState::Running { .. }) {
            return Effect::None;
        }
        self.auto_restart(Stage::Capture, reason, now)
    }

    fn auto_restart(&mut self, stage: Stage, reason: &str, now: Instant) -> Effect {
        self.restarts
            .retain(|at| now.duration_since(*at) < STORM_WINDOW);
        if self.restarts.len() >= STORM_LIMIT {
            let message = format!(
                "{stage:?} failed repeatedly ({} restarts in {}s): {reason}",
                self.restarts.len(),
                STORM_WINDOW.as_secs()
            );
            error!("{message} — giving up until a manual Start");
            self.state = SupervisorState::Failed { reason: message };
            return Effect::StopPipeline;
        }
        self.restarts.push(now);
        self.attempt += 1;
        let backoff = BACKOFF_BASE
            .saturating_mul(1u32 << (self.attempt - 1).min(6))
            .min(BACKOFF_MAX);
        warn!(
            ?stage,
            reason,
            attempt = self.attempt,
            backoff_ms = backoff.as_millis() as u64,
            "Pipeline stage failed — automatic restart scheduled"
        );
        self.state = SupervisorState::BackingOff {
            until: now + backoff,
            attempt: self.attempt,
        };
        Effect::StopPipeline
    }

    /// Called on ticks while backing off; returns Start when the wait is over.
    pub fn on_tick(&mut self, now: Instant) -> Effect {
        if let SupervisorState::BackingOff { until, .. } = self.state {
            if now >= until {
                return Effect::Start;
            }
        }
        Effect::None
    }

    pub fn note_started(&mut self, generation: u64) {
        self.state = SupervisorState::Running { generation };
    }

    /// A successful stretch of running clears the failure budget.
    pub fn note_healthy(&mut self) {
        self.attempt = 0;
    }
}

/// Sends a stage-health report; safe to call from any thread. Reports from
/// stale generations are filtered by the machine.
#[derive(Clone)]
pub struct HealthReporter {
    tx: mpsc::Sender<SupMsg>,
    generation: u64,
}

impl HealthReporter {
    pub fn stage_exited(&self, stage: Stage, outcome: StageOutcome) {
        let _ = self.tx.send(SupMsg::Health {
            generation: self.generation,
            stage,
            outcome,
        });
    }
}

/// The runtime loop: health-driven auto-restarts, bounded joins, watchdogs,
/// and Start/Stop support. Consumes the public `SupervisorCommand` channel
/// (GUI + transport) by forwarding it into the internal health channel.
pub fn run(
    listen_port: u16,
    shared: SharedControl,
    gpu_info: GpuInfo,
    command_tx: mpsc::Sender<SupervisorCommand>,
    command_rx: mpsc::Receiver<SupervisorCommand>,
) {
    let (sup_tx, sup_rx) = mpsc::channel::<SupMsg>();
    {
        let forward = sup_tx.clone();
        std::thread::Builder::new()
            .name("supervisor-cmd-forwarder".into())
            .spawn(move || {
                for command in command_rx {
                    if forward.send(SupMsg::Command(command)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn command forwarder");
    }

    let mut generation = CURRENT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let mut machine = Machine::new(generation);
    let mut pipeline = Some(spawn_generation(
        generation,
        listen_port,
        shared.clone(),
        gpu_info.clone(),
        sup_tx.clone(),
        command_tx.clone(),
    ));
    let mut healthy_since = Instant::now();

    loop {
        let message = sup_rx.recv_timeout(Duration::from_millis(250));
        let now = Instant::now();

        // A minute of health clears the storm budget.
        if matches!(machine.state, SupervisorState::Running { .. })
            && now.duration_since(healthy_since) > Duration::from_secs(60)
        {
            machine.note_healthy();
            healthy_since = now;
        }

        let effect = match message {
            Ok(SupMsg::Command(command)) => machine.on_command(&command),
            Ok(SupMsg::Health {
                generation: g,
                stage,
                outcome,
            }) => machine.on_stage_exit(g, stage, &outcome, now),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let ticked = machine.on_tick(now);
                if ticked != Effect::None {
                    ticked
                } else if let Some(reason) = wedge_reason(&shared, &machine) {
                    machine.on_wedged(&reason, now)
                } else {
                    Effect::None
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match effect {
            Effect::None => {}
            Effect::StopPipeline => {
                shared.stop();
                bounded_join(&mut pipeline, generation);
            }
            Effect::Restart | Effect::Start => {
                shared.stop();
                bounded_join(&mut pipeline, generation);
                {
                    let mut stats = PIPELINE_STATS.lock();
                    stats.reset_for_restart();
                    stats.set_target_addr(shared.target_addr.lock().to_string());
                    stats.set_bitrate(shared.abr_current_bps.load(Ordering::SeqCst));
                }
                generation = CURRENT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
                machine.note_started(generation);
                healthy_since = Instant::now();
                pipeline = Some(spawn_generation(
                    generation,
                    listen_port,
                    shared.clone(),
                    gpu_info.clone(),
                    sup_tx.clone(),
                    command_tx.clone(),
                ));
            }
            Effect::Shutdown => {
                shared.stop();
                bounded_join(&mut pipeline, generation);
                break;
            }
        }
    }
    info!("Supervisor loop ended");
}

/// Watchdog rules over the heartbeat atomics. Only meaningful while Running
/// and after the pipeline has had a moment to warm up.
fn wedge_reason(shared: &SharedControl, machine: &Machine) -> Option<String> {
    if !matches!(machine.state, SupervisorState::Running { .. }) {
        return None;
    }
    let now_ms = crate::clock::host_now_us() / 1000;
    let loop_ms = shared.hb_capture_loop_ms.load(Ordering::Relaxed);
    let frame_ms = shared.hb_capture_frame_ms.load(Ordering::Relaxed);
    let encode_ms = shared.hb_encode_frame_ms.load(Ordering::Relaxed);

    // Capture loop stopped iterating entirely (a wedged AcquireNextFrame /
    // driver call): the loop stamps every iteration including idle timeouts.
    if loop_ms > 0 && now_ms.saturating_sub(loop_ms) > 3_000 {
        return Some(format!(
            "capture loop silent for {}ms",
            now_ms.saturating_sub(loop_ms)
        ));
    }

    // A client is connected but no frame has been produced for 5s. Sound
    // because the idle keepalive guarantees a frame at least every ~750ms
    // while a client is registered.
    let client_connected = {
        let addr = *shared.target_addr.lock();
        !addr.ip().is_unspecified() && addr.port() != 0
    };
    if client_connected && frame_ms > 0 && now_ms.saturating_sub(frame_ms) > 5_000 {
        return Some(format!(
            "no captured frame for {}ms with a client connected",
            now_ms.saturating_sub(frame_ms)
        ));
    }

    // Frames flow but nothing comes out of the encoder: encoder wedged.
    if frame_ms > 0
        && encode_ms > 0
        && frame_ms.saturating_sub(encode_ms) > 3_000
        && now_ms.saturating_sub(encode_ms) > 3_000
    {
        return Some(format!(
            "encoder silent for {}ms while capture advances",
            now_ms.saturating_sub(encode_ms)
        ));
    }
    None
}

fn spawn_generation(
    generation: u64,
    listen_port: u16,
    shared: SharedControl,
    gpu_info: GpuInfo,
    sup_tx: mpsc::Sender<SupMsg>,
    command_tx: mpsc::Sender<SupervisorCommand>,
) -> std::thread::JoinHandle<()> {
    shared.running.store(true, Ordering::SeqCst);
    {
        let mut stats = PIPELINE_STATS.lock();
        stats.mark_pipeline_started();
        stats.set_bitrate(shared.abr_current_bps.load(Ordering::SeqCst));
        stats.set_target_addr(shared.target_addr.lock().to_string());
    }
    let reporter = HealthReporter {
        tx: sup_tx,
        generation,
    };
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(error) => {
                reporter.stage_exited(
                    Stage::Transport,
                    StageOutcome::Failed(format!("tokio runtime: {error}")),
                );
                return;
            }
        };
        rt.block_on(async move {
            crate::pipeline::run_pipeline_supervised(
                generation,
                listen_port,
                shared,
                gpu_info,
                command_tx,
                reporter,
            )
            .await;
        });
    })
}

/// Join with a deadline; on timeout, DETACH — the generation counter already
/// poisoned the zombie (its health reports are filtered and its channel sends
/// fail as the new generation's stages replace the endpoints).
fn bounded_join(pipeline: &mut Option<std::thread::JoinHandle<()>>, generation: u64) {
    let Some(handle) = pipeline.take() else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            error!(
                generation,
                "Pipeline generation refused to exit within 5s — detaching (poisoned)"
            );
            return; // handle dropped => detached
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if let Err(panic) = handle.join() {
        error!(generation, ?panic, "Pipeline thread panicked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(reason: &str) -> StageOutcome {
        StageOutcome::Failed(reason.to_string())
    }

    #[test]
    fn stage_failure_backs_off_then_restarts() {
        let mut machine = Machine::new(1);
        let t0 = Instant::now();

        let effect = machine.on_stage_exit(1, Stage::Capture, &failed("dxgi died"), t0);
        assert_eq!(effect, Effect::StopPipeline);
        assert!(matches!(machine.state, SupervisorState::BackingOff { .. }));

        // Before the backoff elapses: no start.
        assert_eq!(
            machine.on_tick(t0 + Duration::from_millis(100)),
            Effect::None
        );
        // After: start.
        assert_eq!(
            machine.on_tick(t0 + Duration::from_millis(600)),
            Effect::Start
        );
        machine.note_started(2);
        assert!(matches!(
            machine.state,
            SupervisorState::Running { generation: 2 }
        ));
    }

    #[test]
    fn stale_generation_reports_are_ignored() {
        let mut machine = Machine::new(5);
        let effect = machine.on_stage_exit(3, Stage::Encoder, &failed("zombie"), Instant::now());
        assert_eq!(effect, Effect::None);
        assert!(matches!(machine.state, SupervisorState::Running { .. }));
    }

    #[test]
    fn restart_storm_lands_in_failed_until_manual_start() {
        let mut machine = Machine::new(1);
        let t0 = Instant::now();
        let mut generation = 1;
        for i in 0..STORM_LIMIT {
            let at = t0 + Duration::from_secs(i as u64 * 5);
            let effect = machine.on_stage_exit(generation, Stage::Encoder, &failed("boom"), at);
            assert_eq!(effect, Effect::StopPipeline, "failure {i}");
            generation += 1;
            machine.note_started(generation);
        }
        // The storm-limit-th+1 failure inside the window trips the brake.
        let effect = machine.on_stage_exit(
            generation,
            Stage::Encoder,
            &failed("boom"),
            t0 + Duration::from_secs(30),
        );
        assert_eq!(effect, Effect::StopPipeline);
        assert!(matches!(machine.state, SupervisorState::Failed { .. }));

        // Ticks do nothing in Failed; a manual Start recovers.
        assert_eq!(machine.on_tick(t0 + Duration::from_secs(600)), Effect::None);
        assert_eq!(machine.on_command(&SupervisorCommand::Start), Effect::Start);
    }

    #[test]
    fn stop_then_start_cycle() {
        let mut machine = Machine::new(1);
        assert_eq!(
            machine.on_command(&SupervisorCommand::Stop),
            Effect::StopPipeline
        );
        assert!(matches!(machine.state, SupervisorState::Stopped));
        // Failures while stopped are ignored (they're the pipeline winding down).
        assert_eq!(
            machine.on_stage_exit(1, Stage::Capture, &failed("late"), Instant::now()),
            Effect::None
        );
        assert_eq!(machine.on_command(&SupervisorCommand::Start), Effect::Start);
    }

    #[test]
    fn wedge_detection_restarts() {
        let mut machine = Machine::new(1);
        let effect = machine.on_wedged("no frames for 5s", Instant::now());
        assert_eq!(effect, Effect::StopPipeline);
        assert!(matches!(machine.state, SupervisorState::BackingOff { .. }));
    }

    #[test]
    fn healthy_stretch_resets_the_budget() {
        let mut machine = Machine::new(1);
        let t0 = Instant::now();
        let mut generation = 1;
        for i in 0..STORM_LIMIT - 1 {
            machine.on_stage_exit(
                generation,
                Stage::Encoder,
                &failed("flap"),
                t0 + Duration::from_secs(i as u64),
            );
            generation += 1;
            machine.note_started(generation);
        }
        machine.note_healthy();
        // Old restarts still in the window count toward the storm, but the
        // budget reset keeps backoff small; verify we can still restart
        // rather than land in Failed after a healthy stretch + one failure
        // outside the window.
        let effect = machine.on_stage_exit(
            generation,
            Stage::Encoder,
            &failed("one more"),
            t0 + STORM_WINDOW + Duration::from_secs(1),
        );
        assert_eq!(effect, Effect::StopPipeline);
        assert!(matches!(machine.state, SupervisorState::BackingOff { .. }));
    }
}
