//! Single-client protocol-v2 session state machine.
//!
//! Pure logic with an injected clock — the transport task feeds it inbound
//! control messages and time ticks; it returns actions (reply datagrams,
//! shared-state changes) for the transport to execute. This keeps every
//! rule (busy rejection, supersede-in-place, duplicate-ACK idempotency,
//! liveness expiry, keyframe-request rate limiting) unit-testable without
//! sockets.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{
    ByeReason, ControlMessage, HelloAck, HelloStatus, InputEvent, KeyframeRequest, ReceiverReport,
    StreamConfig, FEATURE_WANTS_INPUT,
};
use tracing::{info, warn};

/// Host-dictated timing, advertised to the client in HELLO_ACK.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);
pub const REPORT_INTERVAL_MS: u16 = 500;
pub const LIVENESS_TIMEOUT: Duration = Duration::from_millis(3000);
/// Honor at most one keyframe request per this window (PLI storm guard).
pub const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// What the transport must do after feeding the session an event.
#[derive(Debug, Default)]
pub struct Actions {
    /// Serialized control datagrams to send, with their destination.
    pub replies: Vec<(SocketAddr, Vec<u8>)>,
    /// The media target changed (new session or takeover): update
    /// `SharedControl::target_addr` and reset connection stats.
    pub new_target: Option<SocketAddr>,
    /// Ask the encoder for an IDR (new/superseded session, keyframe request).
    pub force_idr: bool,
    /// The client is gone (BYE or liveness expiry): stop sending media and, if
    /// the virtual display is in use, restart the pipeline so it tears down.
    pub client_lost: bool,
    /// A fresh receiver report arrived (feeds the ABR controller).
    pub report: Option<ReceiverReport>,
    /// A validated `(session_id, event)` from the connected client (only
    /// produced when that client's HELLO2 asked for input relay). The
    /// transport dedupes per session and injects.
    pub input: Option<(u32, InputEvent)>,
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub device_name: String,
    pub screen_px: (u16, u16),
    pub refresh_hz: u8,
    pub decoder_caps: u16,
    pub feature_caps: u16,
    pub connected_at: Instant,
}

struct ActiveSession {
    session_id: u32,
    client_nonce: u32,
    peer: SocketAddr,
    info: ClientInfo,
    liveness_deadline: Instant,
    last_keyframe_grant: Option<Instant>,
    last_report: Option<ReceiverReport>,
    msg_seq_out: u32,
}

/// Provides the current stream parameters for HELLO_ACK/heartbeats.
pub trait ConfigSource {
    fn stream_config(&self) -> StreamConfig;
    fn host_name(&self) -> String;
}

pub struct Session {
    active: Option<ActiveSession>,
    /// Injected so tests control randomness; production passes a seeded value
    /// derived from process entropy.
    next_session_id: u32,
    legacy_client_warned: bool,
}

impl Session {
    pub fn new(session_id_seed: u32) -> Self {
        Self {
            active: None,
            next_session_id: session_id_seed.max(1),
            legacy_client_warned: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn session_id(&self) -> Option<u32> {
        self.active.as_ref().map(|s| s.session_id)
    }

    pub fn peer(&self) -> Option<SocketAddr> {
        self.active.as_ref().map(|s| s.peer)
    }

    pub fn client_info(&self) -> Option<ClientInfo> {
        self.active.as_ref().map(|s| s.info.clone())
    }

    /// Whether the connected client advertised HEVC decode in its HELLO2.
    /// False with no active session — no client, no reason to encode HEVC.
    pub fn client_supports_hevc(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|s| s.info.decoder_caps & eternal_wire::v2::control::CAP_DECODE_HEVC != 0)
    }

    pub fn last_report(&self) -> Option<ReceiverReport> {
        self.active.as_ref().and_then(|s| s.last_report)
    }

    fn allocate_session_id(&mut self) -> u32 {
        let id = self.next_session_id;
        self.next_session_id = self.next_session_id.wrapping_add(0x9E37_79B9).max(1);
        id
    }

    fn next_msg_seq(&mut self) -> u32 {
        match self.active.as_mut() {
            Some(session) => {
                session.msg_seq_out = session.msg_seq_out.wrapping_add(1).max(1);
                session.msg_seq_out
            }
            None => 1,
        }
    }

    /// Feed one inbound control message. `now` is injected for testability.
    pub fn handle_control(
        &mut self,
        source: SocketAddr,
        message: ControlMessage,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        match message {
            ControlMessage::Hello2(hello) => self.handle_hello(source, hello, config, now),
            ControlMessage::Bye(reason) => self.handle_bye(source, reason),
            ControlMessage::KeyframeRequest(request) => self.handle_keyframe(source, request, now),
            ControlMessage::ReceiverReport(report) => self.handle_report(source, report, now),
            ControlMessage::Ping(ping) => self.handle_ping(source, ping, now),
            ControlMessage::InputEvent(event) => self.handle_input(source, event, now),
            // Host-outbound types arriving inbound: ignore.
            _ => Actions::default(),
        }
    }

    fn handle_input(&mut self, source: SocketAddr, event: InputEvent, now: Instant) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source) {
            return actions;
        }
        // Only relay input the client declared it wants to send — a session
        // that connected view-only stays view-only until it re-handshakes.
        if session.info.feature_caps & FEATURE_WANTS_INPUT == 0 {
            return actions;
        }
        // A stream of touches is proof of life as good as any report.
        session.liveness_deadline = now + LIVENESS_TIMEOUT;
        actions.input = Some((session.session_id, event));
        actions
    }

    fn handle_hello(
        &mut self,
        source: SocketAddr,
        hello: eternal_wire::v2::control::Hello2,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();

        // Version gate first.
        if hello.proto_min > 2 || hello.proto_max < 2 {
            let ack = self.make_ack(
                HelloStatus::VersionUnsupported,
                hello.client_nonce,
                0,
                config,
            );
            actions.replies.push((source, ack));
            return actions;
        }

        // The media target: the client's advertised listen port at its source IP
        // (normally identical to the source port; 0 = malformed, use the source).
        let media_port = if hello.listen_port != 0 {
            hello.listen_port
        } else {
            source.port()
        };
        let media_target = SocketAddr::new(source.ip(), media_port);

        if let Some(session) = self.active.as_ref() {
            if session.info_matches_peer(source) {
                if session.client_nonce == hello.client_nonce {
                    // Pure retransmit: re-send the identical ACK.
                    let (session_id, nonce) = (session.session_id, session.client_nonce);
                    let ack = self.make_ack(HelloStatus::Ok, nonce, session_id, config);
                    actions.replies.push((source, ack));
                    return actions;
                }
                // Same device, new connect attempt: supersede in place.
                info!(peer = %source, "Client reconnected — superseding session in place");
            } else {
                // A different device while one is streaming: reject.
                info!(peer = %source, "Second client rejected while a session is active");
                let ack = self.make_ack(HelloStatus::Busy, hello.client_nonce, 0, config);
                actions.replies.push((source, ack));
                return actions;
            }
        }

        let session_id = self.allocate_session_id();
        info!(
            peer = %source,
            session_id,
            device = %hello.device_name,
            screen = format!("{}x{}", hello.screen_px_w, hello.screen_px_h),
            "Client session established"
        );
        self.active = Some(ActiveSession {
            session_id,
            client_nonce: hello.client_nonce,
            peer: source,
            info: ClientInfo {
                device_name: hello.device_name.clone(),
                screen_px: (hello.screen_px_w, hello.screen_px_h),
                refresh_hz: hello.refresh_hz,
                decoder_caps: hello.decoder_caps,
                feature_caps: hello.feature_caps,
                connected_at: now,
            },
            liveness_deadline: now + LIVENESS_TIMEOUT,
            last_keyframe_grant: None,
            last_report: None,
            msg_seq_out: 0,
        });

        let ack = self.make_ack(HelloStatus::Ok, hello.client_nonce, session_id, config);
        actions.replies.push((source, ack));
        actions.new_target = Some(media_target);
        actions.force_idr = true;
        actions
    }

    fn handle_bye(&mut self, source: SocketAddr, reason: ByeReason) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_ref() else {
            return actions;
        };
        if !session.info_matches_peer(source) {
            return actions;
        }
        info!(peer = %source, ?reason, "Client said goodbye");
        self.active = None;
        actions.client_lost = true;
        actions
    }

    fn handle_keyframe(
        &mut self,
        source: SocketAddr,
        request: KeyframeRequest,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source) {
            return actions;
        }
        session.liveness_deadline = now + LIVENESS_TIMEOUT;

        let granted = match session.last_keyframe_grant {
            Some(last) if now.duration_since(last) < KEYFRAME_REQUEST_MIN_INTERVAL => false,
            _ => true,
        };
        if granted {
            session.last_keyframe_grant = Some(now);
            info!(reason = ?request.reason, "Keyframe requested by client — forcing IDR");
            actions.force_idr = true;
        }
        actions
    }

    fn handle_report(
        &mut self,
        source: SocketAddr,
        report: ReceiverReport,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        if let Some(session) = self.active.as_mut() {
            if session_peer_matches(session, source) {
                session.liveness_deadline = now + LIVENESS_TIMEOUT;
                session.last_report = Some(report);
                actions.report = Some(report);
            }
        }
        actions
    }

    fn handle_ping(
        &mut self,
        source: SocketAddr,
        ping: eternal_wire::v2::control::Ping,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_mut() else {
            return actions;
        };
        if !session_peer_matches(session, source) {
            return actions;
        }
        session.liveness_deadline = now + LIVENESS_TIMEOUT;

        let t2 = crate::clock::host_now_us();
        let session_id = session.session_id;
        let msg_seq = self.next_msg_seq();
        let pong = ControlMessage::Pong(eternal_wire::v2::control::Pong {
            t1_us: ping.t1_us,
            t2_us: t2,
            t3_us: crate::clock::host_now_us(),
        });
        actions.replies.push((
            source,
            eternal_wire::v2::control::encode_control(session_id, msg_seq, &pong),
        ));
        actions
    }

    /// Periodic tick: expire the session when the client has been silent past
    /// the liveness window. Returns the heartbeat to send (if a session is
    /// active and `send_heartbeat` is true).
    pub fn tick(
        &mut self,
        config: &impl ConfigSource,
        send_heartbeat: bool,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let Some(session) = self.active.as_ref() else {
            return actions;
        };

        if now >= session.liveness_deadline {
            warn!(
                peer = %session.peer,
                device = %session.info.device_name,
                "Client liveness expired — tearing session down"
            );
            self.active = None;
            actions.client_lost = true;
            return actions;
        }

        if send_heartbeat {
            let peer = session.peer;
            let session_id = session.session_id;
            let msg_seq = self.next_msg_seq();
            let heartbeat = ControlMessage::Heartbeat(eternal_wire::v2::control::Heartbeat {
                host_time_us: crate::clock::host_now_us(),
                stream_config: config.stream_config(),
            });
            actions.replies.push((
                peer,
                eternal_wire::v2::control::encode_control(session_id, msg_seq, &heartbeat),
            ));
        }
        actions
    }

    /// One STREAM_CONFIG notify for the active client (bitrate/fps/resolution
    /// changed). The heartbeat's embedded config self-heals if this is lost.
    pub fn stream_config_notify(
        &mut self,
        config: &impl ConfigSource,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        let Some(session) = self.active.as_ref() else {
            return Vec::new();
        };
        let peer = session.peer;
        let session_id = session.session_id;
        let msg_seq = self.next_msg_seq();
        let message = ControlMessage::StreamConfig(config.stream_config());
        vec![(
            peer,
            eternal_wire::v2::control::encode_control(session_id, msg_seq, &message),
        )]
    }

    /// A legacy (v0.1.x) ETERNALHELLO arrived: no wire reply — the old app
    /// can't parse anything we'd send — but tell the user what happened.
    pub fn note_legacy_hello(&mut self, source: SocketAddr) {
        if !self.legacy_client_warned {
            self.legacy_client_warned = true;
            warn!(
                peer = %source,
                "A v0.1.x iPad app tried to connect. Protocol v2 is a clean break — \
                 update the iPad app to stream again."
            );
        }
    }

    fn make_ack(
        &mut self,
        status: HelloStatus,
        client_nonce: u32,
        session_id: u32,
        config: &impl ConfigSource,
    ) -> Vec<u8> {
        let msg_seq = self.next_msg_seq();
        let ack = ControlMessage::HelloAck(HelloAck {
            status,
            accepted_version: 2,
            client_nonce,
            session_id,
            heartbeat_interval_ms: HEARTBEAT_INTERVAL.as_millis() as u16,
            report_interval_ms: REPORT_INTERVAL_MS,
            liveness_timeout_ms: LIVENESS_TIMEOUT.as_millis() as u16,
            stream_config: config.stream_config(),
            host_name: config.host_name(),
        });
        eternal_wire::v2::control::encode_control(session_id, msg_seq, &ack)
    }
}

impl ActiveSession {
    fn info_matches_peer(&self, source: SocketAddr) -> bool {
        // Same device = same IP. The source PORT may change across app
        // relaunches (ephemeral binds), which must supersede, not reject.
        self.peer.ip() == source.ip()
    }
}

fn session_peer_matches(session: &ActiveSession, source: SocketAddr) -> bool {
    session.peer.ip() == source.ip()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::control::{Hello2, KeyframeReason, CAP_DECODE_H264};

    struct TestConfig;

    impl ConfigSource for TestConfig {
        fn stream_config(&self) -> StreamConfig {
            StreamConfig {
                stream_epoch: 7,
                width: 1280,
                height: 720,
                fps: 60,
                codec: 0,
                flags: 0,
                bitrate_bps: 15_000_000,
            }
        }

        fn host_name(&self) -> String {
            "TEST-HOST".to_string()
        }
    }

    fn hello(nonce: u32, port: u16) -> ControlMessage {
        ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: nonce,
            listen_port: port,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: 0,
            screen_px_w: 2420,
            screen_px_h: 1668,
            screen_pt_w: 1210,
            screen_pt_h: 834,
            refresh_hz: 120,
            device_name: "Test iPad".to_string(),
        })
    }

    fn addr(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::from((ip, port))
    }

    fn parse_ack(bytes: &[u8]) -> HelloAck {
        let (_, message) = eternal_wire::v2::control::parse_control(bytes).unwrap();
        match message {
            ControlMessage::HelloAck(ack) => ack,
            other => panic!("expected ack, got {other:?}"),
        }
    }

    #[test]
    fn first_hello_establishes_session_and_targets_media() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);

        let actions = session.handle_control(peer, hello(1, 50000), &TestConfig, now);

        assert_eq!(actions.replies.len(), 1);
        let ack = parse_ack(&actions.replies[0].1);
        assert_eq!(ack.status, HelloStatus::Ok);
        assert_ne!(ack.session_id, 0);
        assert_eq!(ack.liveness_timeout_ms, 3000);
        assert_eq!(actions.new_target, Some(addr([10, 0, 0, 5], 50000)));
        assert!(actions.force_idr);
        assert!(session.is_active());

        let info = session.client_info().unwrap();
        assert_eq!(info.screen_px, (2420, 1668));
        assert_eq!(
            info.refresh_hz, 120,
            "panel refresh feeds the VDD mode list"
        );
    }

    #[test]
    fn duplicate_nonce_gets_identical_ack_without_new_session() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);

        let first = session.handle_control(peer, hello(1, 50000), &TestConfig, now);
        let first_id = parse_ack(&first.replies[0].1).session_id;

        let dup = session.handle_control(peer, hello(1, 50000), &TestConfig, now);
        assert_eq!(parse_ack(&dup.replies[0].1).session_id, first_id);
        assert!(dup.new_target.is_none(), "retransmit must not re-target");
        assert!(!dup.force_idr);
    }

    #[test]
    fn new_nonce_from_same_ip_supersedes_in_place() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let first = session.handle_control(peer, hello(1, 50000), &TestConfig, now);
        let first_id = parse_ack(&first.replies[0].1).session_id;

        // App relaunched: new ephemeral source port, new nonce.
        let relaunched = addr([10, 0, 0, 5], 50101);
        let second = session.handle_control(relaunched, hello(2, 50101), &TestConfig, now);
        let second_id = parse_ack(&second.replies[0].1).session_id;

        assert_ne!(
            second_id, first_id,
            "supersede must mint a fresh session id"
        );
        assert_eq!(second.new_target, Some(addr([10, 0, 0, 5], 50101)));
        assert!(second.force_idr);
    }

    #[test]
    fn different_ip_is_rejected_busy_while_active() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        session.handle_control(
            addr([10, 0, 0, 5], 50000),
            hello(1, 50000),
            &TestConfig,
            now,
        );

        let intruder = addr([10, 0, 0, 9], 40000);
        let actions = session.handle_control(intruder, hello(9, 40000), &TestConfig, now);
        let ack = parse_ack(&actions.replies[0].1);
        assert_eq!(ack.status, HelloStatus::Busy);
        assert_eq!(ack.session_id, 0);
        assert!(actions.new_target.is_none());
        assert_eq!(session.peer(), Some(addr([10, 0, 0, 5], 50000)));
    }

    #[test]
    fn unsupported_version_is_refused() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let msg = ControlMessage::Hello2(Hello2 {
            proto_min: 3,
            proto_max: 4,
            client_nonce: 1,
            listen_port: 50000,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: 0,
            screen_px_w: 1,
            screen_px_h: 1,
            screen_pt_w: 1,
            screen_pt_h: 1,
            refresh_hz: 60,
            device_name: String::new(),
        });
        let actions = session.handle_control(peer, msg, &TestConfig, now);
        assert_eq!(
            parse_ack(&actions.replies[0].1).status,
            HelloStatus::VersionUnsupported
        );
        assert!(!session.is_active());
    }

    #[test]
    fn bye_and_liveness_expiry_tear_down() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control(peer, hello(1, 50000), &TestConfig, now);

        let bye = session.handle_control(
            peer,
            ControlMessage::Bye(ByeReason::UserDisconnect),
            &TestConfig,
            now,
        );
        assert!(bye.client_lost);
        assert!(!session.is_active());

        // Re-establish, then let liveness lapse.
        session.handle_control(peer, hello(2, 50000), &TestConfig, now);
        let expired = session.tick(&TestConfig, false, now + LIVENESS_TIMEOUT);
        assert!(expired.client_lost);
        assert!(!session.is_active());
    }

    #[test]
    fn reports_extend_liveness_and_are_stored() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control(peer, hello(1, 50000), &TestConfig, now);

        let later = now + Duration::from_millis(2500);
        let mut report = ReceiverReport::default();
        report.frames_complete = 99;
        session.handle_control(
            peer,
            ControlMessage::ReceiverReport(report),
            &TestConfig,
            later,
        );

        // Would have expired at now+3s without the report.
        let ticked = session.tick(&TestConfig, false, now + Duration::from_millis(4000));
        assert!(!ticked.client_lost);
        assert_eq!(session.last_report().unwrap().frames_complete, 99);

        // Expires 3s after the report.
        let expired = session.tick(&TestConfig, false, later + LIVENESS_TIMEOUT);
        assert!(expired.client_lost);
    }

    #[test]
    fn keyframe_requests_are_rate_limited() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control(peer, hello(1, 50000), &TestConfig, now);

        let request = ControlMessage::KeyframeRequest(KeyframeRequest {
            stream_epoch: 7,
            last_complete_seq: 10,
            reason: KeyframeReason::GapLoss,
        });

        let first = session.handle_control(peer, request.clone(), &TestConfig, now);
        assert!(first.force_idr);

        let spammed = session.handle_control(
            peer,
            request.clone(),
            &TestConfig,
            now + Duration::from_millis(100),
        );
        assert!(
            !spammed.force_idr,
            "requests inside the window are coalesced"
        );

        let granted_again =
            session.handle_control(peer, request, &TestConfig, now + Duration::from_millis(600));
        assert!(granted_again.force_idr);
    }

    fn input_event(event_id: u32) -> ControlMessage {
        ControlMessage::InputEvent(InputEvent {
            input_ver: 1,
            kind: 0,
            phase: 0,
            buttons: 1,
            event_id,
            x_norm: 100,
            y_norm: 100,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: 0,
            modifiers: 0,
            client_time_us: 0,
        })
    }

    #[test]
    fn input_relayed_only_for_sessions_that_asked() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);

        // View-only session: input is dropped.
        session.handle_control(peer, hello(1, 50000), &TestConfig, now);
        let dropped = session.handle_control(peer, input_event(1), &TestConfig, now);
        assert!(
            dropped.input.is_none(),
            "view-only sessions must not inject"
        );

        // Re-handshake asking for input relay.
        let mut wants = match hello(2, 50000) {
            ControlMessage::Hello2(h) => h,
            _ => unreachable!(),
        };
        wants.feature_caps = FEATURE_WANTS_INPUT;
        session.handle_control(peer, ControlMessage::Hello2(wants), &TestConfig, now);

        let relayed = session.handle_control(peer, input_event(2), &TestConfig, now);
        assert!(relayed.input.is_some());

        // A different IP can't inject into this session.
        let intruder = addr([10, 0, 0, 9], 40000);
        let foreign = session.handle_control(intruder, input_event(3), &TestConfig, now);
        assert!(foreign.input.is_none());
    }

    #[test]
    fn input_extends_liveness() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        let peer = addr([10, 0, 0, 5], 50000);
        let mut wants = match hello(1, 50000) {
            ControlMessage::Hello2(h) => h,
            _ => unreachable!(),
        };
        wants.feature_caps = FEATURE_WANTS_INPUT;
        session.handle_control(peer, ControlMessage::Hello2(wants), &TestConfig, now);

        // A drag mid-window keeps the session alive past the original deadline.
        let later = now + Duration::from_millis(2500);
        session.handle_control(peer, input_event(1), &TestConfig, later);
        let ticked = session.tick(&TestConfig, false, now + Duration::from_millis(4000));
        assert!(!ticked.client_lost);
    }

    #[test]
    fn heartbeats_flow_only_while_active() {
        let mut session = Session::new(1234);
        let now = Instant::now();
        assert!(session.tick(&TestConfig, true, now).replies.is_empty());

        let peer = addr([10, 0, 0, 5], 50000);
        session.handle_control(peer, hello(1, 50000), &TestConfig, now);
        let ticked = session.tick(&TestConfig, true, now + Duration::from_millis(100));
        assert_eq!(ticked.replies.len(), 1);
        let (_, message) = eternal_wire::v2::control::parse_control(&ticked.replies[0].1).unwrap();
        assert!(matches!(message, ControlMessage::Heartbeat(_)));
    }
}
