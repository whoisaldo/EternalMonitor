use std::sync::mpsc;

use eternal_host::control::{GuiControl, SharedControl};
use eternal_host::pipeline::DEFAULT_BITRATE_BPS;
use eternal_host::{capture, clock, discovery, gpu, gui, logging, stats, vdd};
use tracing::{error, info};
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Anchor the process-wide media clock before anything else runs.
    clock::init();

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=warn"));
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stdout.with_max_level(tracing::Level::INFO))
        .with_filter(logging::MdnsDedupFilter::new());
    let memory_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(logging::MemoryLogWriter::new)
        .with_filter(logging::MdnsDedupFilter::new());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(memory_layer)
        .init();

    info!(
        path = %logging::session_log_path().display(),
        "Session log file initialized"
    );

    // Never leave a virtual display attached across runs. If a previous run crashed or was
    // force-killed while streaming the virtual extended display, the VDD device is still enabled
    // and shows up as a phantom monitor. Disabling it unconditionally at startup guarantees the
    // managed display only ever exists while EternalMonitor is actively using it.
    vdd::disable();

    // Belt-and-suspenders: a panic anywhere (GUI, supervisor, pipeline threads) must still tear
    // the virtual display down before the process unwinds, so a crash can't strand a phantom
    // monitor. The default panic hook still runs afterwards for logging/backtrace.
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            vdd::disable();
            default_hook(info);
        }));
    }

    let listen_port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(9876);

    // Initialize FFmpeg early so encoder probing works during GPU detection
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        return Err(e.into());
    }

    let gpu_info = gpu::GpuInfo::detect();

    // Best-effort capture source for the banner — reflects the default (primary). The
    // authoritative per-run source (including a persisted/fallback selection) is logged
    // inside the capture loop when the stream starts.
    let capture_summary = {
        let outputs = capture::enumerate_outputs();
        outputs
            .iter()
            .find(|o| o.is_primary)
            .or_else(|| outputs.first())
            .map(|o| format!("{} ({}x{})", o.device_name, o.width, o.height))
            .unwrap_or_else(|| "unknown".to_string())
    };

    info!("══════════════════════════════════");
    info!("  EternalMonitor v{}", env!("CARGO_PKG_VERSION"));
    info!("  GPU:     {} ({})", gpu_info.name, gpu_info.vendor);
    info!("  VRAM:    {} MB", gpu_info.dedicated_vram_mb);
    info!("  Encoder: {}", gpu_info.codec_display_name);
    info!("  Capture: {}", capture_summary);
    info!("  Listen:  0.0.0.0:{}", listen_port);
    info!("══════════════════════════════════");

    // Load persisted settings BEFORE the first pipeline spawns, so generation
    // 0 already streams with the user's bitrate/fps/encoder/display/codec.
    // (Previously only the GUI applied them, seconds later — and headless
    // runs never did.) The GUI re-applies the same values at startup, which
    // is idempotent.
    let persisted = eternal_host::settings::SettingsFile::load();
    let initial_bitrate = if persisted.bitrate_mbps > 0.0 {
        (persisted.bitrate_mbps * 1_000_000.0).round() as u32
    } else {
        DEFAULT_BITRATE_BPS
    };

    let shared = SharedControl::new(listen_port, initial_bitrate);
    if persisted.target_fps == 30 || persisted.target_fps == 60 {
        shared
            .target_fps
            .store(persisted.target_fps, std::sync::atomic::Ordering::SeqCst);
    }
    if let Some(name) = persisted.encoder_override.clone() {
        *shared.encoder_override.lock() = Some(name);
    }
    *shared.capture_target.lock() =
        eternal_host::control::CaptureTarget::from_setting(persisted.capture_display.as_deref());
    shared
        .hevc_enabled
        .store(persisted.hevc_enabled, std::sync::atomic::Ordering::SeqCst);

    {
        let mut stats = stats::PIPELINE_STATS.lock();
        stats.set_bitrate(initial_bitrate);
        stats.set_listen_addr(gui::detect_local_ip(listen_port));
        stats.set_target_addr(shared.target_addr.lock().to_string());
        stats.set_gpu_name(gpu_info.name.clone());
        stats.set_codec_name(gpu_info.codec_display_name.clone());
    }

    // Optional encoder override from the environment, applied BEFORE the first pipeline run
    // so it takes effect without a manual stream restart. Useful for testing a specific
    // encoder, e.g. `set ETERNAL_ENCODER=h264_amf` to force the AMD path.
    if let Ok(name) = std::env::var("ETERNAL_ENCODER") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            info!(encoder = %name, "Encoder forced from ETERNAL_ENCODER (no restart needed)");
            *shared.encoder_override.lock() = Some(name);
        }
    }
    // Optional HEVC preference override for automation (the E2E harness runs headless without
    // the GUI toggle). When present it is authoritative in BOTH directions, so
    // ETERNAL_HEVC=0 pins H.264 regardless of the persisted setting.
    if let Ok(value) = std::env::var("ETERNAL_HEVC") {
        let enabled = value.trim() == "1";
        info!(enabled, "HEVC preference forced from ETERNAL_HEVC");
        shared
            .hevc_enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }
    // Optional target FPS override (e.g. ETERNAL_FPS=30) to lighten encode load — useful when
    // testing a hardware encoder on a weak/integrated GPU.
    if let Ok(fps) = std::env::var("ETERNAL_FPS") {
        if let Ok(fps) = fps.trim().parse::<u32>() {
            if fps > 0 {
                info!(fps, "Target FPS forced from ETERNAL_FPS");
                shared
                    .target_fps
                    .store(fps, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let gui_control = GuiControl {
        shared: shared.clone(),
        supervisor_tx: supervisor_tx.clone(),
    };

    /// Set by the termination-signal handler; the headless loop polls it.
    /// (On Windows no handler is registered — headless there still exits only
    /// on a hard kill, same as before.)
    static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    #[cfg(unix)]
    extern "C" fn on_terminate_signal(_signal: libc::c_int) {
        // Async-signal-safe: only a flag store — the polling loop does the work.
        SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    let supervisor_thread = std::thread::spawn(move || {
        eternal_host::supervisor::run(listen_port, shared, gpu_info, supervisor_tx, supervisor_rx);
    });

    let mdns = discovery::advertise_service(listen_port);

    // Headless mode for automation (the iOS E2E harness runs the host without
    // a window): stream until SIGTERM/SIGINT, then fall through to the same
    // clean teardown as a GUI quit — mDNS goodbye, VDD disable, bounded
    // supervisor join. (Previously a kill skipped all of that.)
    if std::env::var("ETERNAL_HEADLESS").is_ok_and(|v| v.trim() == "1") {
        info!("ETERNAL_HEADLESS=1 — running without GUI until terminated");
        #[cfg(unix)]
        unsafe {
            let handler = on_terminate_signal as extern "C" fn(libc::c_int);
            libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
            libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        }
        while !SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        info!("Termination signal received — shutting down cleanly");
    } else if let Err(e) = gui::run_gui(gui_control.clone()) {
        error!(error = %e, "GUI exited with error");
    }

    gui_control.shared.stop();
    gui_control.request_shutdown();

    // Never leave the virtual display attached after we exit — it should only exist while
    // EternalMonitor is actively using it.
    vdd::disable();

    // Give the supervisor a bounded amount of time to shut down cleanly,
    // exiting the moment it finishes instead of always burning the full
    // window. If it doesn't finish (blocked on DXGI acquire, etc.),
    // force-exit so the process never lingers as an invisible zombie.
    let (supervisor_done_tx, supervisor_done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Err(error) = supervisor_thread.join() {
            error!(error = ?error, "Supervisor thread panicked");
        }
        let _ = supervisor_done_tx.send(());
    });

    // Say goodbye on mDNS while the supervisor winds down, so iPads drop the
    // host from their scan list promptly (process::exit skips destructors —
    // without this no goodbye is ever sent).
    if let Some(mdns) = mdns {
        discovery::say_goodbye(mdns);
    }

    if supervisor_done_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .is_err()
    {
        tracing::warn!("Supervisor did not shut down within 3s — forcing exit");
    }

    info!("EternalMonitor shutting down");
    std::process::exit(0);
}
