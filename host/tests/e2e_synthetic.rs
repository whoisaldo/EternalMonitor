//! End-to-end pipeline test: the REAL supervisor + capture(synthetic) +
//! encoder(libx264) + UDP transport, talked to by a fake receiver that speaks
//! the same wire protocol as the iPad — including the registration handshake
//! that triggers a full pipeline restart, exactly like a first iPad connect.
//!
//! Proves, on any dev machine or CI runner with FFmpeg: frames flow through
//! capture → encode → fragment → UDP → reassembly → FlatBuffer parse → H.264
//! decode, the first delivered frame is a parameter-set-bearing keyframe, and
//! the decoded pictures carry advancing frame counters (i.e. the video path
//! is real, not just bytes moving).

use std::net::UdpSocket;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eternal_host::capture::synthetic::decode_frame_counter_from_luma;
use eternal_host::control::{SharedControl, SupervisorCommand};
use eternal_host::gpu::GpuInfo;
use eternal_host::pipeline;
use eternal_wire::frame::parse_frame_packet;
use eternal_wire::h264::contains_nal_type;
use eternal_wire::reassembly::{AddOutcome, Reassembler};
use eternal_wire::v1_fragment::{FragmentHeader, HEADER_SIZE};

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

#[test]
fn synthetic_stream_end_to_end() {
    let _ = ffmpeg_next::init();

    // Deterministic pipeline: synthetic source at a small size, software encoder.
    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    // Run the REAL supervisor so the registration-triggered pipeline restart
    // path is exercised, not mocked.
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        pipeline::supervisor_loop(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    // ---- Fake iPad ----
    let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver socket");
    receiver
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("read timeout");
    let receiver_port = receiver.local_addr().unwrap().port();

    // Register like the iPad does: ETERNALHELLO + listen port, sent a few times.
    let mut hello = Vec::from(*b"ETERNALHELLO");
    hello.extend_from_slice(&receiver_port.to_le_bytes());
    let host_addr = format!("127.0.0.1:{listen_port}");

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut datagram = [0u8; 2048];

    let mut first_frame_checked = false;
    let mut decoded_counters: Vec<u64> = Vec::new();
    let mut last_hello = Instant::now() - Duration::from_secs(1);

    while decoded_counters.len() < WANT_DECODED_FRAMES {
        assert!(
            Instant::now() < deadline,
            "timed out with {} decoded frames (reassembly counters: {:?})",
            decoded_counters.len(),
            reassembler.counters()
        );

        // Re-send HELLO until media starts flowing (covers the restart window
        // where the transport socket is briefly down — same as the iPad's 3x
        // HELLO burst, just more persistent).
        if decoded_counters.is_empty() && last_hello.elapsed() >= Duration::from_millis(300) {
            receiver.send_to(&hello, &host_addr).expect("send hello");
            last_hello = Instant::now();
        }

        let (len, _) = match receiver.recv_from(&mut datagram) {
            Ok(ok) => ok,
            Err(_) => continue, // timeout — loop re-sends hello / re-checks deadline
        };
        if len < HEADER_SIZE {
            continue;
        }

        let header_bytes: [u8; HEADER_SIZE] = datagram[..HEADER_SIZE].try_into().unwrap();
        let header = FragmentHeader::from_bytes(&header_bytes);
        let payload = &datagram[HEADER_SIZE..len];

        let outcome = reassembler.add_fragment(
            header.seq,
            header.fragment_index,
            header.fragment_count,
            header.stream_epoch,
            payload,
            Instant::now(),
        );

        if let AddOutcome::Completed(frame_bytes) = outcome {
            let packet = parse_frame_packet(&frame_bytes).expect("valid FramePacket");
            assert_eq!(packet.width, SYNTH_W);
            assert_eq!(packet.height, SYNTH_H);

            if !first_frame_checked {
                first_frame_checked = true;
                assert!(
                    packet.is_keyframe,
                    "first delivered frame must be a keyframe"
                );
                assert!(
                    contains_nal_type(&packet.data, 7),
                    "startup keyframe must carry an SPS"
                );
                assert!(
                    contains_nal_type(&packet.data, 8),
                    "startup keyframe must carry a PPS"
                );
                assert!(
                    contains_nal_type(&packet.data, 5),
                    "startup keyframe must carry an IDR slice"
                );
            }

            for counter in decoder.decode(&packet.data) {
                // The counter equals the capture-side frame number (mod 24 bits).
                assert_eq!(
                    counter,
                    packet.seq as u64 & 0xFF_FFFF,
                    "decoded frame counter must match the wire sequence number"
                );
                decoded_counters.push(counter);
            }
        }
    }

    // Counters must advance monotonically — real video, in order.
    for pair in decoded_counters.windows(2) {
        assert!(
            pair[1] > pair[0],
            "decoded frame counters must strictly increase: {:?}",
            pair
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
