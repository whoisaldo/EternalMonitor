//! End-to-end pipeline test over protocol v2: the REAL supervisor +
//! capture(synthetic) + encoder(libx264) + UDP transport, talked to by a fake
//! receiver speaking the same wire protocol as the iPad.
//!
//! Proves, on any dev machine or CI runner with FFmpeg:
//! - HELLO2/HELLO_ACK session establishment (nonzero session id, host timing),
//! - media flows as v2 datagrams stamped with that session id,
//! - the first delivered frame is a parameter-set-bearing keyframe,
//! - decoded pictures carry advancing frame counters (real video end to end),
//! - host heartbeats arrive while streaming,
//! - a client KEYFRAME_REQUEST forces an IDR ahead of the natural GOP,
//! - BYE stops the media stream promptly,
//! - shutdown is bounded.

use std::net::UdpSocket;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eternal_host::capture::synthetic::decode_frame_counter_from_luma;
use eternal_host::control::{SharedControl, SupervisorCommand};
use eternal_host::gpu::GpuInfo;
use eternal_host::pipeline;
use eternal_wire::h264::contains_nal_type;
use eternal_wire::reassembly::{AddOutcome, Reassembler};
use eternal_wire::v2::control::{
    encode_control, parse_control, ByeReason, ControlMessage, Hello2, HelloAck, HelloStatus,
    KeyframeReason, KeyframeRequest, ReceiverReport, CAP_DECODE_H264,
};
use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified};

/// Tests share process-global env (ETERNAL_*); run them one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Surface the host's tracing in test output (best effort, once per process).
fn init_test_tracing() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}

const SYNTH_W: u32 = 640;
const SYNTH_H: u32 = 360;
const WANT_DECODED_FRAMES: usize = 30;
const DEADLINE: Duration = Duration::from_secs(30);

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .expect("ephemeral port")
}

struct H264TestDecoder {
    decoder: ffmpeg_next::codec::decoder::Video,
    frame: ffmpeg_next::frame::Video,
}

impl H264TestDecoder {
    fn new() -> Self {
        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264).expect("h264 decoder");
        let context = ffmpeg_next::codec::Context::new_with_codec(codec);
        let decoder = context.decoder().video().expect("open h264 decoder");
        Self {
            decoder,
            frame: ffmpeg_next::frame::Video::empty(),
        }
    }

    /// Feed one Annex B access unit; returns the counter values of any frames
    /// that came out.
    fn decode(&mut self, annex_b: &[u8]) -> Vec<u64> {
        let packet = ffmpeg_next::Packet::copy(annex_b);
        if self.decoder.send_packet(&packet).is_err() {
            return Vec::new();
        }
        let mut counters = Vec::new();
        while self.decoder.receive_frame(&mut self.frame).is_ok() {
            let stride = self.frame.stride(0);
            let luma = self.frame.data(0);
            counters.push(decode_frame_counter_from_luma(luma, stride, SYNTH_W));
        }
        counters
    }
}

/// The fake iPad: one socket, HELLO2 handshake, media reassembly, reports.
struct FakeReceiver {
    socket: UdpSocket,
    host: String,
    session_id: u32,
    msg_seq: u32,
    last_report: Instant,
}

impl FakeReceiver {
    fn connect(host_port: u16) -> Self {
        Self::connect_with_caps(host_port, 0)
    }

    fn connect_with_caps(host_port: u16, feature_caps: u16) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("receiver socket");
        socket
            .set_read_timeout(Some(Duration::from_millis(150)))
            .expect("read timeout");
        let listen_port = socket.local_addr().unwrap().port();
        let host = format!("127.0.0.1:{host_port}");

        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 0xE2E0_0001,
            listen_port,
            decoder_caps: CAP_DECODE_H264,
            feature_caps,
            screen_px_w: 2420,
            screen_px_h: 1668,
            screen_pt_w: 1210,
            screen_pt_h: 834,
            refresh_hz: 120,
            device_name: "E2E fake iPad".to_string(),
        });
        let hello_bytes = encode_control(0, 1, &hello);

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut buf = [0u8; 2048];
        let mut last_hello = Instant::now() - Duration::from_secs(1);
        let mut datagrams_seen = 0u32;
        let mut media_seen = 0u32;
        let ack: HelloAck = loop {
            assert!(
                Instant::now() < deadline,
                "no HELLO_ACK within 20s (saw {datagrams_seen} datagrams, {media_seen} media)"
            );
            if last_hello.elapsed() >= Duration::from_millis(250) {
                socket.send_to(&hello_bytes, &host).expect("send hello2");
                last_hello = Instant::now();
            }
            let Ok((len, _)) = socket.recv_from(&mut buf) else {
                continue;
            };
            datagrams_seen += 1;
            match classify(&buf[..len]) {
                Classified::Control(_) => {
                    if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buf[..len]) {
                        break ack;
                    }
                }
                Classified::Media { .. } => media_seen += 1,
                _ => {}
            }
        };

        assert_eq!(ack.status, HelloStatus::Ok);
        assert_ne!(ack.session_id, 0, "an OK ack must carry a session id");
        assert_eq!(ack.accepted_version, 2);
        assert_eq!(ack.liveness_timeout_ms, 3000);
        assert!(!ack.host_name.is_empty());

        Self {
            socket,
            host,
            session_id: ack.session_id,
            msg_seq: 1,
            last_report: Instant::now(),
        }
    }

    fn send(&mut self, message: &ControlMessage) {
        self.msg_seq += 1;
        let bytes = encode_control(self.session_id, self.msg_seq, message);
        self.socket
            .send_to(&bytes, &self.host)
            .expect("send control");
    }

    /// Keep the host's liveness window open (reports double as keepalive).
    fn maybe_report(&mut self, highest_seq: u32, frames_complete: u32) {
        if self.last_report.elapsed() >= Duration::from_millis(400) {
            self.last_report = Instant::now();
            self.send(&ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 0,
                highest_seq,
                frames_complete,
                ..Default::default()
            }));
        }
    }
}

#[test]
fn synthetic_stream_end_to_end_v2() {
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::remove_var("ETERNAL_DROP");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect(listen_port);

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut datagram = [0u8; 2048];

    let mut first_frame_checked = false;
    let mut decoded_counters: Vec<u64> = Vec::new();
    let mut heartbeats_seen = 0u32;
    let mut keyframe_requested_at: Option<u64> = None;
    let mut forced_keyframe_seen = false;
    let mut highest_seq = 0u32;

    while decoded_counters.len() < WANT_DECODED_FRAMES
        || !forced_keyframe_seen
        || heartbeats_seen == 0
    {
        assert!(
            Instant::now() < deadline,
            "timed out: {} decoded frames, forced_keyframe_seen={forced_keyframe_seen}, \
             heartbeats={heartbeats_seen} (reassembly: {:?})",
            decoded_counters.len(),
            reassembler.counters()
        );

        receiver.maybe_report(highest_seq, decoded_counters.len() as u32);

        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            continue;
        };
        let bytes = &datagram[..len];

        match classify(bytes) {
            Classified::Control(_) => {
                if let Ok((_, ControlMessage::Heartbeat(hb))) = parse_control(bytes) {
                    heartbeats_seen += 1;
                    assert_eq!(hb.stream_config.width, SYNTH_W as u16);
                    assert_eq!(hb.stream_config.height, SYNTH_H as u16);
                }
            }
            Classified::Media { .. } => {
                let (header, payload) = MediaHeader::decode(bytes).expect("valid media datagram");
                assert_eq!(
                    header.session_id, receiver.session_id,
                    "media must be stamped with the negotiated session id"
                );
                highest_seq = highest_seq.max(header.frame_seq);

                let outcome = reassembler.add_fragment(
                    header.frame_seq,
                    header.frag_index,
                    header.frag_count,
                    header.stream_epoch,
                    payload,
                    Instant::now(),
                );
                let AddOutcome::Completed(frame_bytes) = outcome else {
                    continue;
                };

                // v2 media payload is raw Annex B — no FlatBuffer wrapper.
                if !first_frame_checked {
                    first_frame_checked = true;
                    assert!(
                        header.is_keyframe,
                        "first delivered frame must be a keyframe"
                    );
                    assert!(
                        contains_nal_type(&frame_bytes, 7),
                        "startup keyframe must carry an SPS"
                    );
                    assert!(
                        contains_nal_type(&frame_bytes, 8),
                        "startup keyframe must carry a PPS"
                    );
                    assert!(
                        contains_nal_type(&frame_bytes, 5),
                        "startup keyframe must carry an IDR slice"
                    );
                }

                if keyframe_requested_at.is_none() && decoded_counters.len() >= 5 {
                    // Ask for an IDR mid-GOP. x264's natural GOP is 30, so a
                    // keyframe well before seq+25 proves the request worked.
                    keyframe_requested_at = Some(header.frame_seq as u64);
                    receiver.send(&ControlMessage::KeyframeRequest(KeyframeRequest {
                        stream_epoch: header.stream_epoch,
                        last_complete_seq: header.frame_seq,
                        reason: KeyframeReason::GapLoss,
                    }));
                }
                if let Some(at) = keyframe_requested_at {
                    if header.is_keyframe
                        && header.frame_seq as u64 > at
                        && (header.frame_seq as u64) < at + 25
                    {
                        forced_keyframe_seen = true;
                    }
                }

                for counter in decoder.decode(&frame_bytes) {
                    assert_eq!(
                        counter,
                        header.frame_seq as u64 & 0xFF_FFFF,
                        "decoded frame counter must match the wire sequence number"
                    );
                    decoded_counters.push(counter);
                }
            }
            other => panic!("unexpected datagram from host: {other:?}"),
        }
    }

    assert!(
        heartbeats_seen >= 1,
        "host heartbeats must arrive while streaming"
    );
    for pair in decoded_counters.windows(2) {
        assert!(
            pair[1] > pair[0],
            "decoded frame counters must strictly increase: {:?}",
            pair
        );
    }

    // ---- BYE stops the media stream promptly ----
    receiver.send(&ControlMessage::Bye(ByeReason::UserDisconnect));
    receiver.send(&ControlMessage::Bye(ByeReason::UserDisconnect));

    let quiet_deadline = Instant::now() + Duration::from_secs(3);
    let mut last_media = Instant::now();
    loop {
        match receiver.socket.recv_from(&mut datagram) {
            Ok((len, _)) => {
                if matches!(classify(&datagram[..len]), Classified::Media { .. }) {
                    last_media = Instant::now();
                }
            }
            Err(_) => {
                if last_media.elapsed() >= Duration::from_millis(800) {
                    break; // media stream went quiet after BYE
                }
            }
        }
        assert!(
            Instant::now() < quiet_deadline,
            "media kept flowing more than 3s after BYE"
        );
    }

    // ---- Clean shutdown within a bounded window ----
    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");

    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}

/// Under injected fragment loss the system must keep delivering decodable
/// video (keyframe requests beat the GOP) and the ABR must step the bitrate
/// down from its 15 Mbps start.
#[test]
fn lossy_stream_recovers_and_adapts() {
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::set_var("ETERNAL_DROP", "0.05");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect(listen_port);

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut datagram = [0u8; 2048];

    let mut decoded = 0usize;
    let mut abr_stepped_down = false;
    let mut keyframe_requests = 0u32;
    let mut last_progress = Instant::now();
    let mut highest_seq = 0u32;
    let mut current_epoch = 0u32;

    while decoded < 40 || !abr_stepped_down {
        assert!(
            Instant::now() < deadline,
            "timed out: decoded={decoded}, abr_stepped_down={abr_stepped_down}, \
             keyframe_requests={keyframe_requests}, counters={:?}",
            reassembler.counters()
        );

        // Real loss accounting: the reassembler's counters feed the reports
        // that drive the host ABR.
        let counters = reassembler.counters();
        if receiver.last_report.elapsed() >= Duration::from_millis(400) {
            receiver.last_report = Instant::now();
            receiver.send(&ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: current_epoch,
                highest_seq,
                frames_complete: counters.frames_complete as u32,
                frames_dropped: counters.frames_dropped as u32,
                frags_received: counters.frags_received as u32,
                frags_lost: counters.frags_lost as u32,
                ..Default::default()
            }));
        }

        // Client-side recovery: stuck for 400ms -> ask for a keyframe.
        if last_progress.elapsed() >= Duration::from_millis(400) {
            last_progress = Instant::now();
            keyframe_requests += 1;
            receiver.send(&ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: current_epoch,
                last_complete_seq: highest_seq,
                reason: KeyframeReason::GapLoss,
            }));
        }

        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            continue;
        };
        let bytes = &datagram[..len];

        match classify(bytes) {
            Classified::Control(_) => {
                if let Ok((_, ControlMessage::Heartbeat(hb))) = parse_control(bytes) {
                    if hb.stream_config.bitrate_bps < 15_000_000 {
                        abr_stepped_down = true;
                    }
                }
            }
            Classified::Media { .. } => {
                let Ok((header, payload)) = MediaHeader::decode(bytes) else {
                    continue;
                };
                highest_seq = highest_seq.max(header.frame_seq);
                current_epoch = header.stream_epoch;
                if let AddOutcome::Completed(frame_bytes) = reassembler.add_fragment(
                    header.frame_seq,
                    header.frag_index,
                    header.frag_count,
                    header.stream_epoch,
                    payload,
                    Instant::now(),
                ) {
                    let frames = decoder.decode(&frame_bytes);
                    if !frames.is_empty() {
                        decoded += frames.len();
                        last_progress = Instant::now();
                    }
                }
            }
            _ => {}
        }
    }

    let counters = reassembler.counters();
    assert!(
        counters.frags_lost > 0,
        "injected drop must surface as fragment loss"
    );

    std::env::remove_var("ETERNAL_DROP");

    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}

/// Input relay, wire to injection: a client that asked for input in HELLO2
/// sends touches/scrolls; the host maps them through the synthetic capture
/// geometry and "injects" them into the off-Windows recorder. Duplicate edges
/// (sent twice for loss tolerance) must inject exactly once.
#[cfg(not(windows))]
#[test]
fn input_relay_maps_touches_end_to_end() {
    use eternal_host::input::{recorder, Injection, KIND_SCROLL, KIND_TOUCH};
    use eternal_wire::v2::control::{InputEvent, FEATURE_WANTS_INPUT};

    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::remove_var("ETERNAL_DROP");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect_with_caps(listen_port, FEATURE_WANTS_INPUT);

    // Wait for one completed frame: capture is then certainly up, which means
    // the capture geometry the relay maps through has been published.
    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut datagram = [0u8; 2048];
    let mut highest_seq = 0u32;
    'media: loop {
        assert!(Instant::now() < deadline, "no media before input phase");
        receiver.maybe_report(highest_seq, 0);
        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            continue;
        };
        if let Classified::Media { .. } = classify(&datagram[..len]) {
            let (header, payload) = MediaHeader::decode(&datagram[..len]).unwrap();
            highest_seq = highest_seq.max(header.frame_seq);
            if let AddOutcome::Completed(_) = reassembler.add_fragment(
                header.frame_seq,
                header.frag_index,
                header.frag_count,
                header.stream_epoch,
                payload,
                Instant::now(),
            ) {
                break 'media;
            }
        }
    }

    let _ = recorder::take(); // start the assertion window clean

    let base = InputEvent {
        input_ver: 1,
        kind: KIND_TOUCH,
        phase: 0, // began
        buttons: 1,
        event_id: 1,
        x_norm: 0,
        y_norm: 0,
        pressure_x1000: 0,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
    };

    // Tap the top-left corner: began ×2 (edge redundancy), ended ×2.
    receiver.send(&ControlMessage::InputEvent(base));
    receiver.send(&ControlMessage::InputEvent(base));
    let ended = InputEvent {
        phase: 2,
        event_id: 2,
        ..base
    };
    receiver.send(&ControlMessage::InputEvent(ended));
    receiver.send(&ControlMessage::InputEvent(ended));
    // Drag to the bottom-right corner (a move), then a two-finger scroll up.
    receiver.send(&ControlMessage::InputEvent(InputEvent {
        phase: 1,
        event_id: 3,
        x_norm: 65_535,
        y_norm: 65_535,
        ..base
    }));
    receiver.send(&ControlMessage::InputEvent(InputEvent {
        kind: KIND_SCROLL,
        phase: 1,
        event_id: 4,
        x_norm: 32_768,
        y_norm: 32_768,
        buttons: 0,
        scroll_dy: -40,
        ..base
    }));

    // Give the loopback datagrams a moment to be routed and injected. The
    // scroll event was sent last, so once its two injections appear (7 total)
    // everything before it has been processed; a short settle then catches
    // any wrongly-injected duplicate.
    let inject_deadline = Instant::now() + Duration::from_secs(5);
    while recorder::peek().len() < 7 && Instant::now() < inject_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(150));
    let recorded = recorder::take();
    assert!(
        recorded.len() >= 7,
        "expected 7 injections within 5s, got {recorded:?}"
    );

    // Tap at (0,0): move + left down, then move + left up — exactly once
    // despite each edge arriving twice.
    assert_eq!(
        &recorded[..4],
        &[
            Injection::MoveAbs { x: 0, y: 0 },
            Injection::LeftDown { x: 0, y: 0 },
            Injection::MoveAbs { x: 0, y: 0 },
            Injection::LeftUp { x: 0, y: 0 },
        ],
        "tap must inject exactly one click (recorded: {recorded:?})"
    );
    // Drag to the far corner maps to the absolute-space maximum.
    assert_eq!(
        recorded[4],
        Injection::MoveAbs {
            x: 65_535,
            y: 65_535
        }
    );
    // Scroll: pointer placed at ~center, then a wheel tick down.
    let Injection::MoveAbs { x, y } = recorded[5] else {
        panic!("expected scroll pointer move, got {:?}", recorded[5]);
    };
    assert!((i32::from(x) - 32_768).unsigned_abs() < 300, "scroll x {x}");
    assert!((i32::from(y) - 32_768).unsigned_abs() < 300, "scroll y {y}");
    assert_eq!(recorded[6], Injection::Wheel { delta: -120 });
    assert_eq!(recorded.len(), 7, "no extra injections: {recorded:?}");

    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}

/// An encoder crash mid-stream must be contained by the supervisor: automatic
/// restart with backoff, a fresh stream epoch, and the SAME session resuming
/// (the client never re-handshakes; its assembler resets on the epoch bump).
#[test]
fn encoder_crash_auto_restarts_and_stream_recovers() {
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::remove_var("ETERNAL_DROP");
    std::env::set_var("ETERNAL_FAULT_ENCODER_AFTER", "45");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect(listen_port);
    let original_session = receiver.session_id;

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut datagram = [0u8; 2048];

    let mut epochs_seen: Vec<u32> = Vec::new();
    let mut decoded_first_epoch = 0usize;
    let mut decoded_second_epoch = 0usize;
    let mut gap_started: Option<Instant> = None;
    let mut recovery_gap = Duration::ZERO;
    let mut highest_seq = 0u32;

    while decoded_second_epoch < 30 {
        assert!(
            Instant::now() < deadline,
            "timed out: epochs={epochs_seen:?}, first={decoded_first_epoch}, \
             second={decoded_second_epoch}"
        );
        receiver.maybe_report(
            highest_seq,
            (decoded_first_epoch + decoded_second_epoch) as u32,
        );

        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            if gap_started.is_none() && decoded_first_epoch >= 30 {
                gap_started = Some(Instant::now());
            }
            continue;
        };
        let bytes = &datagram[..len];
        let Classified::Media { .. } = classify(bytes) else {
            continue;
        };
        let Ok((header, payload)) = MediaHeader::decode(bytes) else {
            continue;
        };
        assert_eq!(
            header.session_id, original_session,
            "the session must survive the pipeline restart — no re-handshake"
        );
        highest_seq = highest_seq.max(header.frame_seq);
        if !epochs_seen.contains(&header.stream_epoch) {
            epochs_seen.push(header.stream_epoch);
            if epochs_seen.len() == 2 {
                if let Some(started) = gap_started {
                    recovery_gap = started.elapsed();
                }
            }
        }

        if let AddOutcome::Completed(frame_bytes) = reassembler.add_fragment(
            header.frame_seq,
            header.frag_index,
            header.frag_count,
            header.stream_epoch,
            payload,
            Instant::now(),
        ) {
            let frames = decoder.decode(&frame_bytes).len();
            if header.stream_epoch == epochs_seen[0] {
                decoded_first_epoch += frames;
            } else {
                decoded_second_epoch += frames;
            }
        }
    }

    assert!(
        decoded_first_epoch >= 20,
        "should stream normally before the injected crash (got {decoded_first_epoch})"
    );
    assert_eq!(epochs_seen.len(), 2, "exactly one restart expected");
    assert!(
        epochs_seen[1] > epochs_seen[0],
        "the new generation must carry a higher stream epoch"
    );
    assert!(
        recovery_gap < Duration::from_secs(5),
        "recovery took {recovery_gap:?} — the backoff restart should be ~0.5-2s"
    );

    std::env::remove_var("ETERNAL_FAULT_ENCODER_AFTER");
    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}
