//! Golden wire vectors shared with the iOS test suite.
//!
//! `testdata/v2_vectors.txt` holds one canonical datagram per line
//! (`name<space>hex`). This test asserts the Rust encoders reproduce those
//! bytes exactly and the parsers recover the canonical fields; the Swift
//! `WireProtocolGoldenTests` consume the very same file. Together they pin the
//! wire format byte-for-byte across both implementations.
//!
//! To regenerate after an intentional wire change:
//! `UPDATE_GOLDEN=1 cargo test -p eternal-wire --test golden` — then update the
//! Swift-side expected fields to match, deliberately, in the same commit.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use eternal_wire::v2::control::*;
use eternal_wire::v2::media::{MediaHeader, MEDIA_HEADER_SIZE};
use eternal_wire::v2::{classify, Classified, PacketType};

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/v2_vectors.txt")
}

fn stream_config() -> StreamConfig {
    StreamConfig {
        stream_epoch: 3,
        width: 2560,
        height: 1440,
        fps: 60,
        codec: CODEC_H264,
        flags: 0,
        bitrate_bps: 15_000_000,
    }
}

/// The canonical vector set. Every control message type plus both media flag
/// states, with distinctive field values (no zero-heavy defaults that could
/// hide endianness or offset mistakes).
fn canonical_vectors() -> Vec<(&'static str, Vec<u8>)> {
    let mut vectors: Vec<(&'static str, Vec<u8>)> = Vec::new();

    let mut media_key = vec![0u8; MEDIA_HEADER_SIZE + 4];
    MediaHeader {
        session_id: 0xA1B2_C3D4,
        stream_epoch: 7,
        frame_seq: 12_345,
        frag_index: 2,
        frag_count: 9,
        is_keyframe: true,
        capture_ts_us: 0x0000_0123_4567_89AB,
        payload_len: 4,
    }
    .encode_into(&mut media_key);
    media_key[MEDIA_HEADER_SIZE..].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    vectors.push(("media_keyframe", media_key));

    let mut media_delta = vec![0u8; MEDIA_HEADER_SIZE + 2];
    MediaHeader {
        session_id: 0xA1B2_C3D4,
        stream_epoch: 7,
        frame_seq: 12_346,
        frag_index: 0,
        frag_count: 1,
        is_keyframe: false,
        capture_ts_us: 999_999,
        payload_len: 2,
    }
    .encode_into(&mut media_delta);
    media_delta[MEDIA_HEADER_SIZE..].copy_from_slice(&[0x41, 0x9A]);
    vectors.push(("media_delta", media_delta));

    let control: Vec<(&'static str, u32, u32, ControlMessage)> = vec![
        (
            "hello2",
            0,
            1,
            ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 0xDEAD_BEEF,
                listen_port: 9876,
                decoder_caps: CAP_DECODE_H264 | CAP_DECODE_HEVC,
                feature_caps: FEATURE_WANTS_INPUT,
                screen_px_w: 2420,
                screen_px_h: 1668,
                screen_pt_w: 1210,
                screen_pt_h: 834,
                refresh_hz: 120,
                device_name: "Ali's iPad Pro".to_string(),
            }),
        ),
        (
            "hello_ack_ok",
            0,
            1,
            ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Ok,
                accepted_version: 2,
                client_nonce: 0xDEAD_BEEF,
                session_id: 0x1234_5678,
                heartbeat_interval_ms: 1000,
                report_interval_ms: 500,
                liveness_timeout_ms: 3000,
                stream_config: stream_config(),
                host_name: "ALI-PC".to_string(),
            }),
        ),
        (
            "hello_ack_busy",
            0,
            2,
            ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Busy,
                accepted_version: 2,
                client_nonce: 0xDEAD_BEEF,
                session_id: 0,
                heartbeat_interval_ms: 1000,
                report_interval_ms: 500,
                liveness_timeout_ms: 3000,
                stream_config: StreamConfig::default(),
                host_name: "ALI-PC".to_string(),
            }),
        ),
        (
            "heartbeat",
            0x1234_5678,
            42,
            ControlMessage::Heartbeat(Heartbeat {
                host_time_us: 987_654_321,
                stream_config: stream_config(),
            }),
        ),
        (
            "bye_background",
            0x1234_5678,
            43,
            ControlMessage::Bye(ByeReason::AppBackground),
        ),
        (
            "keyframe_request",
            0x1234_5678,
            44,
            ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: 3,
                last_complete_seq: 4111,
                reason: KeyframeReason::GapLoss,
            }),
        ),
        (
            "receiver_report",
            0x1234_5678,
            45,
            ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 3,
                highest_seq: 5000,
                frames_complete: 4990,
                frames_dropped: 10,
                frags_received: 120_000,
                frags_lost: 250,
                jitter_us: 2100,
                decode_fps_x10: 599,
                assembler_depth: 1,
                decode_depth: 0,
                e2e_latency_ms_x10: 321,
                rtt_ms_x10: 28,
            }),
        ),
        (
            "ping",
            0x1234_5678,
            46,
            ControlMessage::Ping(Ping {
                t1_us: 0x0102_0304_0506_0708,
            }),
        ),
        (
            "pong",
            0x1234_5678,
            46,
            ControlMessage::Pong(Pong {
                t1_us: 0x0102_0304_0506_0708,
                t2_us: 0x0102_0304_0506_0710,
                t3_us: 0x0102_0304_0506_0720,
            }),
        ),
        (
            "stream_config",
            0x1234_5678,
            47,
            ControlMessage::StreamConfig(stream_config()),
        ),
        (
            "input_touch_move",
            0x1234_5678,
            48,
            ControlMessage::InputEvent(InputEvent {
                input_ver: 1,
                kind: 0,
                phase: 1,
                buttons: 1,
                event_id: 77,
                x_norm: 32_768,
                y_norm: 16_384,
                pressure_x1000: 500,
                scroll_dx: -3,
                scroll_dy: 12,
                keycode: 0,
                modifiers: 0,
                client_time_us: 1_000_001,
            }),
        ),
    ];

    for (name, session_id, msg_seq, message) in control {
        vectors.push((name, encode_control(session_id, msg_seq, &message)));
    }
    vectors
}

fn render(vectors: &[(&'static str, Vec<u8>)]) -> String {
    let mut out = String::new();
    out.push_str(
        "# EternalMonitor wire protocol v2 golden vectors.\n\
         # One canonical datagram per line: `name<space>lowercase-hex`.\n\
         # Consumed byte-for-byte by BOTH `cargo test -p eternal-wire --test golden`\n\
         # and the iOS `WireProtocolGoldenTests`. Regenerate only on a deliberate\n\
         # wire change: UPDATE_GOLDEN=1 cargo test -p eternal-wire --test golden\n",
    );
    for (name, bytes) in vectors {
        let mut hex = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(hex, "{byte:02x}");
        }
        let _ = writeln!(out, "{name} {hex}");
    }
    out
}

fn parse_file(text: &str) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, hex) = line.split_once(' ').expect("name<space>hex");
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
            .collect();
        map.insert(name.to_string(), bytes);
    }
    map
}

#[test]
fn golden_vectors_match_disk() {
    let vectors = canonical_vectors();
    let rendered = render(&vectors);

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(vectors_path().parent().unwrap()).unwrap();
        std::fs::write(vectors_path(), &rendered).unwrap();
        return;
    }

    let on_disk = std::fs::read_to_string(vectors_path())
        .expect("testdata/v2_vectors.txt missing — run with UPDATE_GOLDEN=1 to create");
    let disk = parse_file(&on_disk);

    assert_eq!(
        disk.len(),
        vectors.len(),
        "vector count mismatch between code and testdata"
    );
    for (name, bytes) in &vectors {
        let expected = disk
            .get(*name)
            .unwrap_or_else(|| panic!("vector {name} missing from testdata"));
        assert_eq!(
            expected, bytes,
            "encoder output for {name} diverged from the golden file"
        );
    }
}

#[test]
fn golden_vectors_parse_back_to_canonical_fields() {
    for (name, bytes) in canonical_vectors() {
        match classify(&bytes) {
            Classified::Media { .. } => {
                let (header, payload) =
                    MediaHeader::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(header.session_id, 0xA1B2_C3D4);
                assert_eq!(header.stream_epoch, 7);
                assert!(!payload.is_empty());
            }
            Classified::Control(_) => {
                let (header, message) =
                    parse_control(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                // Round-trip: re-encoding the parsed message must be identical.
                let re = encode_control(header.session_id, header.msg_seq, &message);
                assert_eq!(re, bytes, "{name} re-encode mismatch");
            }
            other => panic!("{name} classified as {other:?}"),
        }
    }
}

#[test]
fn every_golden_vector_truncation_is_rejected() {
    for (name, bytes) in canonical_vectors() {
        for len in 0..bytes.len() {
            let slice = &bytes[..len];
            let ok = match classify(&bytes) {
                Classified::Media { .. } => MediaHeader::decode(slice).is_ok(),
                _ => parse_control(slice).is_ok(),
            };
            assert!(!ok, "{name} truncated to {len} bytes must not parse");
        }
    }
}

#[test]
fn packet_type_registry_is_stable() {
    // These numbers are wire constants. If this test fails you are breaking
    // protocol compatibility — do that only with a version bump.
    assert_eq!(PacketType::Media as u8, 0x01);
    assert_eq!(PacketType::Hello2 as u8, 0x10);
    assert_eq!(PacketType::HelloAck as u8, 0x11);
    assert_eq!(PacketType::Heartbeat as u8, 0x12);
    assert_eq!(PacketType::Bye as u8, 0x13);
    assert_eq!(PacketType::KeyframeRequest as u8, 0x14);
    assert_eq!(PacketType::ReceiverReport as u8, 0x15);
    assert_eq!(PacketType::Ping as u8, 0x16);
    assert_eq!(PacketType::Pong as u8, 0x17);
    assert_eq!(PacketType::StreamConfig as u8, 0x18);
    assert_eq!(PacketType::InputEvent as u8, 0x20);
}
