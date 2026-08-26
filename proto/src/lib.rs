//! eternal-wire: every byte format and pure protocol/bitstream rule shared by
//! the EternalMonitor host, tests, and tooling. No ffmpeg, no Win32 — this
//! crate compiles and tests on every platform.
//!
//! - [`v2`] — the current wire protocol (media fragments + control plane)
//! - [`h264`] — H.264 bitstream normalization/inspection used by the encoder
//! - [`reassembly`] — fragment reassembly mirroring the iPad's semantics
//! - [`v1_fragment`], [`frame`], [`control`] — legacy v1 wire, removed with
//!   the v1 transport path

pub mod control;
pub mod frame;
pub mod h264;
pub mod reassembly;
pub mod v1_fragment;
pub mod v2;
