//! EternalMonitor wire protocol v2.
//!
//! Every v2 datagram — in both directions — starts with the same 8-byte common
//! prefix (all integers little-endian):
//!
//! ```text
//! [0..2]  magic        u16  = 0x4D45 (the bytes "EM": 0x45 0x4D)
//! [2]     version      u8   = 2
//! [3]     packet_type  u8   (see PacketType)
//! [4]     flags        u8   type-specific (media: bit0 = access unit is a keyframe)
//! [5]     reserved     u8   = 0 on send; ignored on receive
//! [6..8]  payload_len  u16  bytes following the fixed header; receivers reject a
//!                           datagram unless payload_len == datagram_len - header_len
//! ```
//!
//! The v1 hello starts with the bytes "ET" (`ETERNALHELLO`), so v1 and v2 traffic
//! diverge at byte 1 and can share one socket unambiguously.
//!
//! Media datagrams extend the prefix to a 32-byte header ([`media::MediaHeader`]);
//! control datagrams extend it to a 16-byte header ([`control::ControlHeader`])
//! followed by one message ([`control::ControlMessage`]).

pub mod control;
pub mod media;

/// "EM" interpreted as a little-endian u16 (bytes 0x45, 0x4D on the wire).
pub const MAGIC: u16 = 0x4D45;
/// Protocol version carried in every datagram.
pub const VERSION: u8 = 2;
/// Size of the common prefix shared by all v2 datagrams.
pub const PREFIX_SIZE: usize = 8;
/// Maximum size of any datagram we send (matches v1's conservative WiFi MTU pick).
pub const MAX_DGRAM_SIZE: usize = 1400;

/// First bytes of the legacy v1 hello ("ETERNALHELLO"). Kept here so the host can
/// recognize old clients and surface a "please update" message instead of silence.
pub const LEGACY_HELLO_MAGIC: &[u8; 12] = b"ETERNALHELLO";

/// Datagram type registry. Values are stable wire constants — never renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Media = 0x01,
    /// Reserved for forward-error-correction parity datagrams. Not sent in v2.0.
    MediaFec = 0x02,
    Hello2 = 0x10,
    HelloAck = 0x11,
    Heartbeat = 0x12,
    Bye = 0x13,
    KeyframeRequest = 0x14,
    ReceiverReport = 0x15,
    Ping = 0x16,
    Pong = 0x17,
    StreamConfig = 0x18,
    InputEvent = 0x20,
    /// Reserved. Not sent in v2.0.
    Error = 0x7F,
}

impl PacketType {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::Media,
            0x02 => Self::MediaFec,
            0x10 => Self::Hello2,
            0x11 => Self::HelloAck,
            0x12 => Self::Heartbeat,
            0x13 => Self::Bye,
            0x14 => Self::KeyframeRequest,
            0x15 => Self::ReceiverReport,
            0x16 => Self::Ping,
            0x17 => Self::Pong,
            0x18 => Self::StreamConfig,
            0x20 => Self::InputEvent,
            0x7F => Self::Error,
            _ => return None,
        })
    }
}

/// Why a datagram failed to parse. Deliberately small: callers mostly count these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Shorter than the fixed header for its (claimed) type.
    Truncated,
    /// Magic bytes are not "EM".
    BadMagic,
    /// Version byte is not [`VERSION`].
    UnsupportedVersion(u8),
    /// Unknown packet_type byte.
    UnknownType(u8),
    /// payload_len does not equal datagram_len - header_len, or a length field
    /// points outside the datagram.
    LengthMismatch,
    /// A field holds a value the protocol forbids (zero fragment count,
    /// fragment index >= count, oversized name, unknown enum value...).
    InvalidField(&'static str),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "datagram truncated"),
            Self::BadMagic => write!(f, "bad magic"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            Self::UnknownType(t) => write!(f, "unknown packet type 0x{t:02X}"),
            Self::LengthMismatch => write!(f, "payload length mismatch"),
            Self::InvalidField(name) => write!(f, "invalid field: {name}"),
        }
    }
}

impl std::error::Error for WireError {}

/// The common 8-byte prefix, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommonPrefix {
    pub packet_type: PacketType,
    pub flags: u8,
    pub payload_len: u16,
}

impl CommonPrefix {
    /// Writes the prefix into `buf[0..8]`.
    pub fn encode_into(self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        buf[2] = VERSION;
        buf[3] = self.packet_type as u8;
        buf[4] = self.flags;
        buf[5] = 0;
        buf[6..8].copy_from_slice(&self.payload_len.to_le_bytes());
    }

    /// Parses the prefix from the front of a datagram. Does not validate
    /// payload_len against the datagram length — the typed header decoders do
    /// that, because header size differs per type.
    pub fn decode(datagram: &[u8]) -> Result<Self, WireError> {
        if datagram.len() < PREFIX_SIZE {
            return Err(WireError::Truncated);
        }
        let magic = u16::from_le_bytes([datagram[0], datagram[1]]);
        if magic != MAGIC {
            return Err(WireError::BadMagic);
        }
        if datagram[2] != VERSION {
            return Err(WireError::UnsupportedVersion(datagram[2]));
        }
        let packet_type =
            PacketType::from_u8(datagram[3]).ok_or(WireError::UnknownType(datagram[3]))?;
        Ok(Self {
            packet_type,
            flags: datagram[4],
            payload_len: u16::from_le_bytes([datagram[6], datagram[7]]),
        })
    }
}

/// Coarse classification used by both ends to route an inbound datagram before
/// any deeper parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classified {
    /// A v2 media fragment — hand to the reassembler ([`media::MediaHeader::decode`]).
    Media { flags: u8 },
    /// A v2 control datagram — hand to [`control::parse_control`].
    Control(PacketType),
    /// A v1 "ETERNALHELLO" registration from an old client.
    LegacyHello,
    /// Not ours (or corrupt): drop, optionally count.
    Unknown,
}

/// Routes a raw datagram without copying.
pub fn classify(datagram: &[u8]) -> Classified {
    if datagram.len() >= LEGACY_HELLO_MAGIC.len()
        && &datagram[..LEGACY_HELLO_MAGIC.len()] == LEGACY_HELLO_MAGIC
    {
        return Classified::LegacyHello;
    }
    match CommonPrefix::decode(datagram) {
        Ok(prefix) => match prefix.packet_type {
            PacketType::Media | PacketType::MediaFec => Classified::Media {
                flags: prefix.flags,
            },
            other => Classified::Control(other),
        },
        Err(_) => Classified::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_round_trip() {
        let prefix = CommonPrefix {
            packet_type: PacketType::Heartbeat,
            flags: 0x01,
            payload_len: 24,
        };
        let mut buf = [0u8; PREFIX_SIZE];
        prefix.encode_into(&mut buf);
        assert_eq!(&buf[0..2], &[0x45, 0x4D], "magic must be the bytes \"EM\"");
        assert_eq!(buf[2], 2);
        assert_eq!(CommonPrefix::decode(&buf).unwrap(), prefix);
    }

    #[test]
    fn rejects_bad_magic_version_and_type() {
        let mut buf = [0u8; PREFIX_SIZE];
        CommonPrefix {
            packet_type: PacketType::Ping,
            flags: 0,
            payload_len: 8,
        }
        .encode_into(&mut buf);

        let mut bad_magic = buf;
        bad_magic[0] = b'X';
        assert_eq!(CommonPrefix::decode(&bad_magic), Err(WireError::BadMagic));

        let mut bad_version = buf;
        bad_version[2] = 1;
        assert_eq!(
            CommonPrefix::decode(&bad_version),
            Err(WireError::UnsupportedVersion(1))
        );

        let mut bad_type = buf;
        bad_type[3] = 0x66;
        assert_eq!(
            CommonPrefix::decode(&bad_type),
            Err(WireError::UnknownType(0x66))
        );

        assert_eq!(CommonPrefix::decode(&buf[..7]), Err(WireError::Truncated));
    }

    #[test]
    fn classifies_legacy_hello_media_control_and_junk() {
        let mut hello = Vec::from(*LEGACY_HELLO_MAGIC);
        hello.extend_from_slice(&9876u16.to_le_bytes());
        assert_eq!(classify(&hello), Classified::LegacyHello);

        let mut media = [0u8; 32];
        CommonPrefix {
            packet_type: PacketType::Media,
            flags: 0b1,
            payload_len: 0,
        }
        .encode_into(&mut media);
        assert_eq!(classify(&media), Classified::Media { flags: 1 });

        let mut ping = [0u8; 24];
        CommonPrefix {
            packet_type: PacketType::Ping,
            flags: 0,
            payload_len: 8,
        }
        .encode_into(&mut ping);
        assert_eq!(classify(&ping), Classified::Control(PacketType::Ping));

        assert_eq!(classify(&[0u8; 3]), Classified::Unknown);
        assert_eq!(classify(&[0xFFu8; 64]), Classified::Unknown);
    }
}
