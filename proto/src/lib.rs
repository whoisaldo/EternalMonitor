//! eternal-wire: every byte format and pure protocol/bitstream rule shared by
//! the EternalMonitor host, tests, and tooling. No ffmpeg, no Win32 — this
//! crate compiles and tests on every platform.
//!
//! - [`v2`] — the wire protocol (media fragments + control plane)
//! - [`h264`] — H.264 bitstream normalization/inspection used by the encoder
//! - [`hevc`] — H.265 NAL classification for the HEVC streaming path
//! - [`reassembly`] — fragment reassembly mirroring the iPad's semantics

pub mod h264;
pub mod hevc;
pub mod reassembly;
pub mod v2;
