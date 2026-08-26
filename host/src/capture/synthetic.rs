//! Synthetic capture source: a deterministic moving test pattern in BGRA.
//!
//! This is the capture backend on non-Windows platforms (there is no DXGI),
//! which makes the whole pipeline — encode, fragment, send — runnable on a
//! development Mac and in CI. It is also selectable on Windows via
//! `ETERNAL_CAPTURE=synthetic` for debugging the pipeline without touching
//! Desktop Duplication.
//!
//! Every frame carries its frame number encoded as a row of black/white
//! squares that survives lossy H.264 encoding, so end-to-end tests can decode
//! frames and assert real progression through the entire stack.

use std::sync::atomic::Ordering;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::info;

use super::{frame_budget_for, RawFrame};
use crate::control::SharedControl;
use crate::stats::PIPELINE_STATS;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;

/// Geometry of the frame-counter marker row (see [`draw_frame_counter`]).
pub const COUNTER_BITS: usize = 24;
pub const COUNTER_SQUARE: usize = 16;
pub const COUNTER_MARGIN: usize = 4;
/// Height of the solid strip that hosts the counter squares.
pub const COUNTER_STRIP_H: usize = COUNTER_SQUARE + 2 * COUNTER_MARGIN;

/// Resolution for the synthetic source: `ETERNAL_SYNTH_SIZE=WxH`, else 1280x720.
fn synthetic_size() -> (u32, u32) {
    if let Ok(spec) = std::env::var("ETERNAL_SYNTH_SIZE") {
        if let Some((w, h)) = spec.trim().split_once(['x', 'X']) {
            if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                if (16..=7680).contains(&w) && (16..=4320).contains(&h) {
                    return (w, h);
                }
            }
        }
    }
    (DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

/// Produce frames until `shared.running` clears or the channel closes.
/// Mirrors the DXGI loop's contract: paced by `target_fps`, records capture
/// stats, ships BGRA `RawFrame`s.
pub fn run_capture_loop(
    tx: mpsc::Sender<RawFrame>,
    shared: SharedControl,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (width, height) = synthetic_size();
    let row_bytes = (width * 4) as usize;
    let mut pixel_buf = vec![0u8; row_bytes * height as usize];
    let mut frame_number: u64 = 0;

    info!(width, height, "Synthetic capture source active");

    loop {
        if !shared.running.load(Ordering::SeqCst) {
            info!("Capture loop stopping on running=false");
            break;
        }

        let frame_start = Instant::now();
        let frame_budget = frame_budget_for(shared.target_fps.load(Ordering::SeqCst));

        frame_number += 1;
        render_synthetic_frame(&mut pixel_buf, width, height, frame_number);

        PIPELINE_STATS.lock().record_capture(width, height);

        let raw_frame = RawFrame {
            frame_number,
            timestamp: frame_start,
            data: pixel_buf.clone(),
            width,
            height,
        };
        if tx.blocking_send(raw_frame).is_err() {
            info!("Channel closed, stopping capture");
            break;
        }

        let elapsed = frame_start.elapsed();
        if elapsed < frame_budget {
            std::thread::sleep(frame_budget - elapsed);
        }
    }

    Ok(())
}

/// Draw the full test pattern for `frame_number` into a BGRA buffer.
pub fn render_synthetic_frame(buf: &mut [u8], width: u32, height: u32, frame_number: u64) {
    let w = width as usize;
    let h = height as usize;
    let row_bytes = w * 4;
    debug_assert!(buf.len() >= row_bytes * h);

    // Slow-moving diagonal gradient background — cheap, deterministic motion
    // that forces the encoder to produce real deltas every frame.
    let phase = (frame_number * 3) as usize;
    for y in COUNTER_STRIP_H.min(h)..h {
        let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..w {
            let v = ((x + y + phase) & 0xFF) as u8;
            let px = &mut row[x * 4..x * 4 + 4];
            px[0] = v; // B
            px[1] = v / 2 + 64; // G
            px[2] = 255 - v; // R
            px[3] = 255;
        }
    }

    // A bright vertical sweep bar so motion is obvious to the eye.
    let bar_x = (phase * 2) % w.max(1);
    let bar_w = (w / 40).max(2);
    for y in COUNTER_STRIP_H.min(h)..h {
        let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
        for x in bar_x..(bar_x + bar_w).min(w) {
            let px = &mut row[x * 4..x * 4 + 4];
            px[0] = 26;
            px[1] = 122;
            px[2] = 255;
            px[3] = 255;
        }
    }

    draw_frame_counter(buf, width, frame_number);
}

/// Paint the counter strip: a black band across the top with [`COUNTER_BITS`]
/// squares; square `i` is white when bit `i` of `frame_number` is set.
/// 16 px squares survive H.264 at any reasonable bitrate.
pub fn draw_frame_counter(buf: &mut [u8], width: u32, frame_number: u64) {
    let w = width as usize;
    let row_bytes = w * 4;
    let strip_rows = COUNTER_STRIP_H.min(buf.len() / row_bytes);

    for y in 0..strip_rows {
        let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
        let (pixels, _) = row.as_chunks_mut::<4>();
        for px in pixels {
            *px = [0, 0, 0, 255];
        }
    }

    for bit in 0..COUNTER_BITS {
        if frame_number & (1 << bit) == 0 {
            continue;
        }
        let x0 = COUNTER_MARGIN + bit * (COUNTER_SQUARE + COUNTER_MARGIN);
        if x0 + COUNTER_SQUARE > w {
            break;
        }
        for y in COUNTER_MARGIN..COUNTER_MARGIN + COUNTER_SQUARE {
            let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
            for x in x0..x0 + COUNTER_SQUARE {
                let px = &mut row[x * 4..x * 4 + 4];
                px[0] = 255;
                px[1] = 255;
                px[2] = 255;
            }
        }
    }
}

/// Recover the frame number from a decoded luma (Y) plane. The inverse of
/// [`draw_frame_counter`] after a lossy encode/decode round trip: samples the
/// center of each square and thresholds against mid-gray.
pub fn decode_frame_counter_from_luma(luma: &[u8], stride: usize, width: u32) -> u64 {
    let w = width as usize;
    let cy = COUNTER_MARGIN + COUNTER_SQUARE / 2;
    let mut value = 0u64;
    for bit in 0..COUNTER_BITS {
        let x0 = COUNTER_MARGIN + bit * (COUNTER_SQUARE + COUNTER_MARGIN);
        let cx = x0 + COUNTER_SQUARE / 2;
        if cx >= w {
            break;
        }
        let sample = luma.get(cy * stride + cx).copied().unwrap_or(0);
        if sample > 128 {
            value |= 1 << bit;
        }
    }
    value
}

/// Same recovery from the original BGRA buffer (for unit tests without a codec).
pub fn decode_frame_counter_from_bgra(buf: &[u8], width: u32) -> u64 {
    let w = width as usize;
    let row_bytes = w * 4;
    let cy = COUNTER_MARGIN + COUNTER_SQUARE / 2;
    let mut value = 0u64;
    for bit in 0..COUNTER_BITS {
        let x0 = COUNTER_MARGIN + bit * (COUNTER_SQUARE + COUNTER_MARGIN);
        let cx = x0 + COUNTER_SQUARE / 2;
        if cx >= w {
            break;
        }
        let at = cy * row_bytes + cx * 4;
        // Any channel works — the squares are pure black/white.
        let sample = buf.get(at + 1).copied().unwrap_or(0);
        if sample > 128 {
            value |= 1 << bit;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_counter_round_trips_through_bgra() {
        let (w, h) = (640u32, 360u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for frame in [1u64, 2, 3, 42, 255, 4095, 0xFF_FFFF] {
            render_synthetic_frame(&mut buf, w, h, frame);
            assert_eq!(
                decode_frame_counter_from_bgra(&buf, w),
                frame & 0xFF_FFFF,
                "counter must round-trip for frame {frame}"
            );
        }
    }

    #[test]
    fn counter_survives_mild_blur() {
        // Simulate encoder softness: average each sampled pixel with neighbors
        // and confirm thresholding still recovers the value.
        let (w, h) = (640u32, 360u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        render_synthetic_frame(&mut buf, w, h, 0b1010_1100_0011);

        let row_bytes = (w * 4) as usize;
        let mut luma = vec![0u8; (w * h) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let g = buf[y * row_bytes + x * 4 + 1] as u16;
                let left = buf[y * row_bytes + x.saturating_sub(1) * 4 + 1] as u16;
                luma[y * w as usize + x] = ((g + left) / 2) as u8;
            }
        }
        assert_eq!(
            decode_frame_counter_from_luma(&luma, w as usize, w),
            0b1010_1100_0011
        );
    }

    #[test]
    fn synthetic_size_parses_and_bounds() {
        // Not touching the env var here (tests run in parallel) — just the default.
        assert_eq!(super::synthetic_size(), (1280, 720));
    }
}
