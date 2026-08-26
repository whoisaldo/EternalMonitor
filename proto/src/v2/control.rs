//! v2 control datagrams: everything that is not media.
//!
//! Layout — a 16-byte control header, then one message body:
//!
//! ```text
//! [0..8]   common prefix   (packet_type = the message discriminator)
//! [8..12]  session_id u32  0 only in HELLO2 (no session assigned yet)
//! [12..16] msg_seq    u32  per-sender monotonic counter across all control
//!                          types, starting at 1; receivers apply latest-wins
//!                          per type and may drop stale ones
//! ```
//!
//! Reliability is per-message-semantics (RTCP model): HELLO2 retries until
//! acked, BYE fires several times, everything else is periodic and supersedes
//! itself. There is no generic retransmit layer.
//!
//! Evolution rule: bodies are append-only. Decoders MUST ignore trailing bytes
//! they do not understand, so fields can be appended without a version bump.

use super::{CommonPrefix, PacketType, WireError};

/// Size of the control header (common prefix included).
pub const CONTROL_HEADER_SIZE: usize = 16;
/// Longest device/host name accepted on the wire, in UTF-8 bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Codec identifiers used in [`StreamConfig`] and [`Hello2::decoder_caps`].
pub const CODEC_H264: u8 = 0;
pub const CODEC_HEVC: u8 = 1;

/// [`Hello2::decoder_caps`] bits.
pub const CAP_DECODE_H264: u16 = 1 << 0;
pub const CAP_DECODE_HEVC: u16 = 1 << 1;
pub const CAP_DECODE_HEVC_10BIT: u16 = 1 << 2;

/// [`Hello2::feature_caps`] bits.
pub const FEATURE_WANTS_INPUT: u16 = 1 << 0;
pub const FEATURE_WANTS_AUDIO: u16 = 1 << 1;

/// [`StreamConfig::flags`] bits.
pub const STREAM_FLAG_SOFTWARE_ENCODER: u8 = 1 << 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlHeader {
    pub packet_type: PacketType,
    pub session_id: u32,
    pub msg_seq: u32,
}

/// Current stream parameters, embedded in HELLO_ACK and HEARTBEAT and sent
/// standalone as STREAM_CONFIG on change. 16 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamConfig {
    pub stream_epoch: u32,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub codec: u8,
    pub flags: u8,
    pub bitrate_bps: u32,
}

pub const STREAM_CONFIG_SIZE: usize = 16;

impl StreamConfig {
    fn encode_into(self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.stream_epoch.to_le_bytes());
        buf[4..6].copy_from_slice(&self.width.to_le_bytes());
        buf[6..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8..10].copy_from_slice(&self.fps.to_le_bytes());
        buf[10] = self.codec;
        buf[11] = self.flags;
        buf[12..16].copy_from_slice(&self.bitrate_bps.to_le_bytes());
    }

    fn decode(buf: &[u8]) -> Result<Self, WireError> {
        if buf.len() < STREAM_CONFIG_SIZE {
            return Err(WireError::Truncated);
        }
        Ok(Self {
            stream_epoch: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            width: u16::from_le_bytes([buf[4], buf[5]]),
            height: u16::from_le_bytes([buf[6], buf[7]]),
            fps: u16::from_le_bytes([buf[8], buf[9]]),
            codec: buf[10],
            flags: buf[11],
            bitrate_bps: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
        })
    }
}

/// Client → host session request. session_id = 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello2 {
    pub proto_min: u8,
    pub proto_max: u8,
    /// Fresh random value per connect attempt; the host echoes it in HELLO_ACK
    /// and treats a duplicate HELLO2 with the same nonce as a retransmit.
    pub client_nonce: u32,
    /// Kept for parity with v1 semantics; normally equals the UDP source port.
    pub listen_port: u16,
    pub decoder_caps: u16,
    pub feature_caps: u16,
    pub screen_px_w: u16,
    pub screen_px_h: u16,
    pub screen_pt_w: u16,
    pub screen_pt_h: u16,
    pub refresh_hz: u8,
    pub device_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HelloStatus {
    Ok = 0,
    Busy = 1,
    VersionUnsupported = 2,
    Error = 3,
}

impl HelloStatus {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Ok,
            1 => Self::Busy,
            2 => Self::VersionUnsupported,
            3 => Self::Error,
            _ => return None,
        })
    }
}

/// Host → client HELLO2 response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloAck {
    pub status: HelloStatus,
    pub accepted_version: u8,
    pub client_nonce: u32,
    /// Nonzero iff status == Ok.
    pub session_id: u32,
    pub heartbeat_interval_ms: u16,
    pub report_interval_ms: u16,
    pub liveness_timeout_ms: u16,
    pub stream_config: StreamConfig,
    pub host_name: String,
}

/// Host → client, every heartbeat interval while a session is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    pub host_time_us: u64,
    pub stream_config: StreamConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ByeReason {
    UserDisconnect = 0,
    AppBackground = 1,
    HostShuttingDown = 2,
    Error = 3,
    Superseded = 4,
}

impl ByeReason {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::UserDisconnect,
            1 => Self::AppBackground,
            2 => Self::HostShuttingDown,
            3 => Self::Error,
            4 => Self::Superseded,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyframeReason {
    GapLoss = 0,
    DecodeError = 1,
    Startup = 2,
    Resume = 3,
}

impl KeyframeReason {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::GapLoss,
            1 => Self::DecodeError,
            2 => Self::Startup,
            3 => Self::Resume,
            _ => return None,
        })
    }
}

/// Client → host request for an IDR (PLI). Idempotent; rate-limited host-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyframeRequest {
    pub stream_epoch: u32,
    pub last_complete_seq: u32,
    pub reason: KeyframeReason,
}

/// Client → host stats, every report interval. Cumulative per epoch — the host
/// diffs consecutive reports. Doubles as the client's liveness signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReceiverReport {
    pub stream_epoch: u32,
    pub highest_seq: u32,
    pub frames_complete: u32,
    pub frames_dropped: u32,
    pub frags_received: u32,
    pub frags_lost: u32,
    /// RFC 3550-style interarrival jitter, microseconds.
    pub jitter_us: u32,
    pub decode_fps_x10: u16,
    pub assembler_depth: u8,
    pub decode_depth: u8,
    /// 0 until clock sync has converged.
    pub e2e_latency_ms_x10: u16,
    pub rtt_ms_x10: u16,
}

pub const RECEIVER_REPORT_SIZE: usize = 36;

/// Client → host clock probe. `t1_us` is the client clock at send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub t1_us: u64,
}

/// Host → client probe reply: echoes t1, adds host receive/send times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub t1_us: u64,
    pub t2_us: u64,
    pub t3_us: u64,
}

/// Touch/pencil/mouse/key event, client → host. See the input module for the
/// coordinate contract (normalized across the displayed video content rect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub input_ver: u8,
    pub kind: u8,
    pub phase: u8,
    pub buttons: u8,
    /// Per-session monotonic; began/ended events are sent twice for loss
    /// tolerance, so the host must dedupe on this id.
    pub event_id: u32,
    pub x_norm: u16,
    pub y_norm: u16,
    pub pressure_x1000: u16,
    pub scroll_dx: i16,
    pub scroll_dy: i16,
    pub keycode: u16,
    pub modifiers: u8,
    pub client_time_us: u64,
}

pub const INPUT_EVENT_SIZE: usize = 30;

/// One parsed control message.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    Hello2(Hello2),
    HelloAck(HelloAck),
    Heartbeat(Heartbeat),
    Bye(ByeReason),
    KeyframeRequest(KeyframeRequest),
    ReceiverReport(ReceiverReport),
    Ping(Ping),
    Pong(Pong),
    StreamConfig(StreamConfig),
    InputEvent(InputEvent),
}

impl ControlMessage {
    fn packet_type(&self) -> PacketType {
        match self {
            Self::Hello2(_) => PacketType::Hello2,
            Self::HelloAck(_) => PacketType::HelloAck,
            Self::Heartbeat(_) => PacketType::Heartbeat,
            Self::Bye(_) => PacketType::Bye,
            Self::KeyframeRequest(_) => PacketType::KeyframeRequest,
            Self::ReceiverReport(_) => PacketType::ReceiverReport,
            Self::Ping(_) => PacketType::Ping,
            Self::Pong(_) => PacketType::Pong,
            Self::StreamConfig(_) => PacketType::StreamConfig,
            Self::InputEvent(_) => PacketType::InputEvent,
        }
    }
}

/// Serializes one control datagram (header + message body).
pub fn encode_control(session_id: u32, msg_seq: u32, message: &ControlMessage) -> Vec<u8> {
    let mut body = Vec::with_capacity(64);
    match message {
        ControlMessage::Hello2(h) => {
            let name = truncated_name(&h.device_name);
            body.extend_from_slice(&[h.proto_min, h.proto_max]);
            body.extend_from_slice(&h.client_nonce.to_le_bytes());
            body.extend_from_slice(&h.listen_port.to_le_bytes());
            body.extend_from_slice(&h.decoder_caps.to_le_bytes());
            body.extend_from_slice(&h.feature_caps.to_le_bytes());
            body.extend_from_slice(&h.screen_px_w.to_le_bytes());
            body.extend_from_slice(&h.screen_px_h.to_le_bytes());
            body.extend_from_slice(&h.screen_pt_w.to_le_bytes());
            body.extend_from_slice(&h.screen_pt_h.to_le_bytes());
            body.push(h.refresh_hz);
            body.push(name.len() as u8);
            body.extend_from_slice(name.as_bytes());
        }
        ControlMessage::HelloAck(a) => {
            let name = truncated_name(&a.host_name);
            body.push(a.status as u8);
            body.push(a.accepted_version);
            body.extend_from_slice(&a.client_nonce.to_le_bytes());
            body.extend_from_slice(&a.session_id.to_le_bytes());
            body.extend_from_slice(&a.heartbeat_interval_ms.to_le_bytes());
            body.extend_from_slice(&a.report_interval_ms.to_le_bytes());
            body.extend_from_slice(&a.liveness_timeout_ms.to_le_bytes());
            let mut cfg = [0u8; STREAM_CONFIG_SIZE];
            a.stream_config.encode_into(&mut cfg);
            body.extend_from_slice(&cfg);
            body.push(name.len() as u8);
            body.extend_from_slice(name.as_bytes());
        }
        ControlMessage::Heartbeat(hb) => {
            body.extend_from_slice(&hb.host_time_us.to_le_bytes());
            let mut cfg = [0u8; STREAM_CONFIG_SIZE];
            hb.stream_config.encode_into(&mut cfg);
            body.extend_from_slice(&cfg);
        }
        ControlMessage::Bye(reason) => body.push(*reason as u8),
        ControlMessage::KeyframeRequest(k) => {
            body.extend_from_slice(&k.stream_epoch.to_le_bytes());
            body.extend_from_slice(&k.last_complete_seq.to_le_bytes());
            body.push(k.reason as u8);
        }
        ControlMessage::ReceiverReport(r) => {
            body.extend_from_slice(&r.stream_epoch.to_le_bytes());
            body.extend_from_slice(&r.highest_seq.to_le_bytes());
            body.extend_from_slice(&r.frames_complete.to_le_bytes());
            body.extend_from_slice(&r.frames_dropped.to_le_bytes());
            body.extend_from_slice(&r.frags_received.to_le_bytes());
            body.extend_from_slice(&r.frags_lost.to_le_bytes());
            body.extend_from_slice(&r.jitter_us.to_le_bytes());
            body.extend_from_slice(&r.decode_fps_x10.to_le_bytes());
            body.push(r.assembler_depth);
            body.push(r.decode_depth);
            body.extend_from_slice(&r.e2e_latency_ms_x10.to_le_bytes());
            body.extend_from_slice(&r.rtt_ms_x10.to_le_bytes());
        }
        ControlMessage::Ping(p) => body.extend_from_slice(&p.t1_us.to_le_bytes()),
        ControlMessage::Pong(p) => {
            body.extend_from_slice(&p.t1_us.to_le_bytes());
            body.extend_from_slice(&p.t2_us.to_le_bytes());
            body.extend_from_slice(&p.t3_us.to_le_bytes());
        }
        ControlMessage::StreamConfig(cfg) => {
            let mut block = [0u8; STREAM_CONFIG_SIZE];
            cfg.encode_into(&mut block);
            body.extend_from_slice(&block);
        }
        ControlMessage::InputEvent(e) => {
            body.push(e.input_ver);
            body.push(e.kind);
            body.push(e.phase);
            body.push(e.buttons);
            body.extend_from_slice(&e.event_id.to_le_bytes());
            body.extend_from_slice(&e.x_norm.to_le_bytes());
            body.extend_from_slice(&e.y_norm.to_le_bytes());
            body.extend_from_slice(&e.pressure_x1000.to_le_bytes());
            body.extend_from_slice(&e.scroll_dx.to_le_bytes());
            body.extend_from_slice(&e.scroll_dy.to_le_bytes());
            body.extend_from_slice(&e.keycode.to_le_bytes());
            body.push(e.modifiers);
            body.push(0); // reserved
            body.extend_from_slice(&e.client_time_us.to_le_bytes());
        }
    }

    let mut datagram = vec![0u8; CONTROL_HEADER_SIZE + body.len()];
    CommonPrefix {
        packet_type: message.packet_type(),
        flags: 0,
        payload_len: body.len() as u16,
    }
    .encode_into(&mut datagram);
    datagram[8..12].copy_from_slice(&session_id.to_le_bytes());
    datagram[12..16].copy_from_slice(&msg_seq.to_le_bytes());
    datagram[CONTROL_HEADER_SIZE..].copy_from_slice(&body);
    datagram
}

/// Parses one control datagram. Enforces the length invariant
/// (`payload_len == datagram_len - 16`) and every field-level invariant, but
/// ignores trailing body bytes beyond the fields it knows (append-only
/// evolution).
pub fn parse_control(datagram: &[u8]) -> Result<(ControlHeader, ControlMessage), WireError> {
    let prefix = CommonPrefix::decode(datagram)?;
    if datagram.len() < CONTROL_HEADER_SIZE {
        return Err(WireError::Truncated);
    }
    if usize::from(prefix.payload_len) != datagram.len() - CONTROL_HEADER_SIZE {
        return Err(WireError::LengthMismatch);
    }

    let header = ControlHeader {
        packet_type: prefix.packet_type,
        session_id: u32::from_le_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        msg_seq: u32::from_le_bytes([datagram[12], datagram[13], datagram[14], datagram[15]]),
    };
    let body = &datagram[CONTROL_HEADER_SIZE..];
    let mut r = Reader::new(body);

    let message = match prefix.packet_type {
        PacketType::Hello2 => {
            let proto_min = r.u8()?;
            let proto_max = r.u8()?;
            let client_nonce = r.u32()?;
            let listen_port = r.u16()?;
            let decoder_caps = r.u16()?;
            let feature_caps = r.u16()?;
            let screen_px_w = r.u16()?;
            let screen_px_h = r.u16()?;
            let screen_pt_w = r.u16()?;
            let screen_pt_h = r.u16()?;
            let refresh_hz = r.u8()?;
            let device_name = r.name()?;
            if proto_min > proto_max {
                return Err(WireError::InvalidField("proto_range"));
            }
            ControlMessage::Hello2(Hello2 {
                proto_min,
                proto_max,
                client_nonce,
                listen_port,
                decoder_caps,
                feature_caps,
                screen_px_w,
                screen_px_h,
                screen_pt_w,
                screen_pt_h,
                refresh_hz,
                device_name,
            })
        }
        PacketType::HelloAck => {
            let status = HelloStatus::from_u8(r.u8()?).ok_or(WireError::InvalidField("status"))?;
            let accepted_version = r.u8()?;
            let client_nonce = r.u32()?;
            let session_id = r.u32()?;
            let heartbeat_interval_ms = r.u16()?;
            let report_interval_ms = r.u16()?;
            let liveness_timeout_ms = r.u16()?;
            let stream_config = StreamConfig::decode(r.take(STREAM_CONFIG_SIZE)?)?;
            let host_name = r.name()?;
            if status == HelloStatus::Ok && session_id == 0 {
                return Err(WireError::InvalidField("session_id"));
            }
            ControlMessage::HelloAck(HelloAck {
                status,
                accepted_version,
                client_nonce,
                session_id,
                heartbeat_interval_ms,
                report_interval_ms,
                liveness_timeout_ms,
                stream_config,
                host_name,
            })
        }
        PacketType::Heartbeat => ControlMessage::Heartbeat(Heartbeat {
            host_time_us: r.u64()?,
            stream_config: StreamConfig::decode(r.take(STREAM_CONFIG_SIZE)?)?,
        }),
        PacketType::Bye => ControlMessage::Bye(
            ByeReason::from_u8(r.u8()?).ok_or(WireError::InvalidField("bye_reason"))?,
        ),
        PacketType::KeyframeRequest => ControlMessage::KeyframeRequest(KeyframeRequest {
            stream_epoch: r.u32()?,
            last_complete_seq: r.u32()?,
            reason: KeyframeReason::from_u8(r.u8()?)
                .ok_or(WireError::InvalidField("keyframe_reason"))?,
        }),
        PacketType::ReceiverReport => ControlMessage::ReceiverReport(ReceiverReport {
            stream_epoch: r.u32()?,
            highest_seq: r.u32()?,
            frames_complete: r.u32()?,
            frames_dropped: r.u32()?,
            frags_received: r.u32()?,
            frags_lost: r.u32()?,
            jitter_us: r.u32()?,
            decode_fps_x10: r.u16()?,
            assembler_depth: r.u8()?,
            decode_depth: r.u8()?,
            e2e_latency_ms_x10: r.u16()?,
            rtt_ms_x10: r.u16()?,
        }),
        PacketType::Ping => ControlMessage::Ping(Ping { t1_us: r.u64()? }),
        PacketType::Pong => ControlMessage::Pong(Pong {
            t1_us: r.u64()?,
            t2_us: r.u64()?,
            t3_us: r.u64()?,
        }),
        PacketType::StreamConfig => {
            ControlMessage::StreamConfig(StreamConfig::decode(r.take(STREAM_CONFIG_SIZE)?)?)
        }
        PacketType::InputEvent => ControlMessage::InputEvent(InputEvent {
            input_ver: r.u8()?,
            kind: r.u8()?,
            phase: r.u8()?,
            buttons: r.u8()?,
            event_id: r.u32()?,
            x_norm: r.u16()?,
            y_norm: r.u16()?,
            pressure_x1000: r.u16()?,
            scroll_dx: r.i16()?,
            scroll_dy: r.i16()?,
            keycode: r.u16()?,
            modifiers: {
                let m = r.u8()?;
                let _reserved = r.u8()?;
                m
            },
            client_time_us: r.u64()?,
        }),
        PacketType::Media | PacketType::MediaFec | PacketType::Error => {
            return Err(WireError::InvalidField("packet_type"));
        }
    };

    Ok((header, message))
}

fn truncated_name(name: &str) -> &str {
    if name.len() <= MAX_NAME_LEN {
        return name;
    }
    // Truncate on a char boundary at or below the cap.
    let mut end = MAX_NAME_LEN;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// Bounds-checked little-endian body reader. Every read either returns the
/// value or `Truncated` — indexing can never panic.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let slice = self
            .data
            .get(self.at..self.at + len)
            .ok_or(WireError::Truncated)?;
        self.at += len;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Result<i16, WireError> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Length-prefixed UTF-8 string with the [`MAX_NAME_LEN`] cap.
    fn name(&mut self) -> Result<String, WireError> {
        let len = usize::from(self.u8()?);
        if len > MAX_NAME_LEN {
            return Err(WireError::InvalidField("name_len"));
        }
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::InvalidField("name_utf8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> StreamConfig {
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

    fn all_messages() -> Vec<ControlMessage> {
        vec![
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
            ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Ok,
                accepted_version: 2,
                client_nonce: 0xDEAD_BEEF,
                session_id: 0x1234_5678,
                heartbeat_interval_ms: 1000,
                report_interval_ms: 500,
                liveness_timeout_ms: 3000,
                stream_config: sample_config(),
                host_name: "ALI-PC".to_string(),
            }),
            ControlMessage::Heartbeat(Heartbeat {
                host_time_us: 987_654_321,
                stream_config: sample_config(),
            }),
            ControlMessage::Bye(ByeReason::AppBackground),
            ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: 3,
                last_complete_seq: 4111,
                reason: KeyframeReason::GapLoss,
            }),
            ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 3,
                highest_seq: 5000,
                frames_complete: 4990,
                frames_dropped: 10,
                frags_received: 120_000,
                frags_lost: 250,
                jitter_us: 2_100,
                decode_fps_x10: 599,
                assembler_depth: 1,
                decode_depth: 0,
                e2e_latency_ms_x10: 321,
                rtt_ms_x10: 28,
            }),
            ControlMessage::Ping(Ping { t1_us: 42 }),
            ControlMessage::Pong(Pong {
                t1_us: 42,
                t2_us: 43,
                t3_us: 44,
            }),
            ControlMessage::StreamConfig(sample_config()),
            ControlMessage::InputEvent(InputEvent {
                input_ver: 1,
                kind: 0,
                phase: 1,
                buttons: 1,
                event_id: 77,
                x_norm: 32768,
                y_norm: 16384,
                pressure_x1000: 500,
                scroll_dx: -3,
                scroll_dy: 12,
                keycode: 0,
                modifiers: 0,
                client_time_us: 1_000_001,
            }),
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for message in all_messages() {
            let session_id = if matches!(message, ControlMessage::Hello2(_)) {
                0
            } else {
                0x1234_5678
            };
            let datagram = encode_control(session_id, 9, &message);
            let (header, parsed) = parse_control(&datagram)
                .unwrap_or_else(|e| panic!("{message:?} failed to parse: {e}"));
            assert_eq!(header.session_id, session_id);
            assert_eq!(header.msg_seq, 9);
            assert_eq!(parsed, message, "round-trip mismatch");
        }
    }

    #[test]
    fn every_truncation_of_every_message_is_rejected() {
        for message in all_messages() {
            let datagram = encode_control(1, 1, &message);
            for len in 0..datagram.len() {
                assert!(
                    parse_control(&datagram[..len]).is_err(),
                    "{message:?} truncated to {len}/{} bytes must not parse",
                    datagram.len()
                );
            }
        }
    }

    #[test]
    fn trailing_bytes_are_ignored_for_forward_compat() {
        let mut datagram = encode_control(1, 1, &ControlMessage::Ping(Ping { t1_us: 5 }));
        datagram.extend_from_slice(&[0xAA, 0xBB]);
        // payload_len must still match, so a forward-compatible sender bumps it.
        let new_len = (datagram.len() - CONTROL_HEADER_SIZE) as u16;
        datagram[6..8].copy_from_slice(&new_len.to_le_bytes());
        let (_, parsed) = parse_control(&datagram).expect("extended body must parse");
        assert_eq!(parsed, ControlMessage::Ping(Ping { t1_us: 5 }));
    }

    #[test]
    fn invalid_enums_and_names_are_rejected() {
        // Unknown bye reason.
        let mut bye = encode_control(1, 1, &ControlMessage::Bye(ByeReason::Error));
        *bye.last_mut().unwrap() = 200;
        assert_eq!(
            parse_control(&bye),
            Err(WireError::InvalidField("bye_reason"))
        );

        // HELLO_ACK with Ok status but zero session id.
        let ack = ControlMessage::HelloAck(HelloAck {
            status: HelloStatus::Ok,
            accepted_version: 2,
            client_nonce: 1,
            session_id: 0,
            heartbeat_interval_ms: 1000,
            report_interval_ms: 500,
            liveness_timeout_ms: 3000,
            stream_config: sample_config(),
            host_name: String::new(),
        });
        // encode_control would happily serialize it; the parser must reject.
        let datagram = encode_control(0, 1, &ack);
        assert_eq!(
            parse_control(&datagram),
            Err(WireError::InvalidField("session_id"))
        );

        // Oversized name length byte.
        let hello = encode_control(
            0,
            1,
            &ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: 9876,
                decoder_caps: CAP_DECODE_H264,
                feature_caps: 0,
                screen_px_w: 100,
                screen_px_h: 100,
                screen_pt_w: 100,
                screen_pt_h: 100,
                refresh_hz: 60,
                device_name: "x".to_string(),
            }),
        );
        let name_len_at = CONTROL_HEADER_SIZE + 21;
        let mut bad = hello.clone();
        bad[name_len_at] = (MAX_NAME_LEN + 1) as u8;
        assert_eq!(
            parse_control(&bad),
            Err(WireError::InvalidField("name_len"))
        );
    }

    #[test]
    fn long_device_names_are_truncated_on_char_boundary() {
        let long_name = "é".repeat(60); // 120 UTF-8 bytes
        let datagram = encode_control(
            0,
            1,
            &ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: 9876,
                decoder_caps: CAP_DECODE_H264,
                feature_caps: 0,
                screen_px_w: 1,
                screen_px_h: 1,
                screen_pt_w: 1,
                screen_pt_h: 1,
                refresh_hz: 60,
                device_name: long_name,
            }),
        );
        let (_, parsed) = parse_control(&datagram).unwrap();
        let ControlMessage::Hello2(hello) = parsed else {
            panic!("wrong type");
        };
        assert!(hello.device_name.len() <= MAX_NAME_LEN);
        assert_eq!(hello.device_name, "é".repeat(32)); // 64 bytes exactly
    }

    #[test]
    fn random_bytes_never_panic() {
        // Deterministic pseudo-random fuzz (no rand dep): xorshift over lengths/bytes.
        let mut state = 0x1234_5678_9ABC_DEFFu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 80) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
            let _ = parse_control(&bytes); // must not panic
            let _ = super::super::media::MediaHeader::decode(&bytes);
            let _ = super::super::classify(&bytes);
        }
    }
}
