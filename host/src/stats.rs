use std::collections::VecDeque;
use std::time::Instant;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

/// Global pipeline statistics shared between pipeline threads and the GUI.
pub static PIPELINE_STATS: Lazy<Mutex<PipelineStats>> =
    Lazy::new(|| Mutex::new(PipelineStats::new()));

const FPS_WINDOW_SECS: f64 = 1.0;
const HISTORY_LEN: usize = 120;

pub struct PipelineStats {
    // Capture
    pub capture_fps: f64,
    pub capture_resolution: (u32, u32),
    pub capture_frame_count: u64,
    capture_timestamps: VecDeque<Instant>,

    // Encoder
    pub encode_fps: f64,
    pub encode_time_us: u128,
    pub encode_frame_count: u64,
    pub nal_bytes_last: usize,
    encode_timestamps: VecDeque<Instant>,

    // Transport
    pub transport_fps: f64,
    pub transport_bytes_sent: u64,
    pub transport_packets_sent: u64,
    pub transport_fragments_sent: u64,
    pub target_addr: String,
    transport_timestamps: VecDeque<Instant>,

    // Bandwidth (bytes per second, rolling 1s window)
    bandwidth_samples: VecDeque<(Instant, u64)>,
    pub bandwidth_bps: f64,

    // Encode time history for chart
    pub encode_time_history: VecDeque<f64>,

    // Pipeline state
    pub pipeline_running: bool,
    pub start_time: Option<Instant>,

    // mDNS
    pub mdns_active: bool,
}

impl PipelineStats {
    fn new() -> Self {
        Self {
            capture_fps: 0.0,
            capture_resolution: (0, 0),
            capture_frame_count: 0,
            capture_timestamps: VecDeque::with_capacity(128),

            encode_fps: 0.0,
            encode_time_us: 0,
            encode_frame_count: 0,
            nal_bytes_last: 0,
            encode_timestamps: VecDeque::with_capacity(128),

            transport_fps: 0.0,
            transport_bytes_sent: 0,
            transport_packets_sent: 0,
            transport_fragments_sent: 0,
            target_addr: String::new(),
            transport_timestamps: VecDeque::with_capacity(128),

            bandwidth_samples: VecDeque::with_capacity(256),
            bandwidth_bps: 0.0,

            encode_time_history: VecDeque::with_capacity(HISTORY_LEN),

            pipeline_running: false,
            start_time: None,

            mdns_active: false,
        }
    }

    pub fn record_capture(&mut self, width: u32, height: u32) {
        let now = Instant::now();
        self.capture_frame_count += 1;
        self.capture_resolution = (width, height);
        self.capture_timestamps.push_back(now);
        self.capture_fps = Self::calc_fps(&mut self.capture_timestamps, now);
    }

    pub fn record_encode(&mut self, encode_us: u128, nal_bytes: usize) {
        let now = Instant::now();
        self.encode_frame_count += 1;
        self.encode_time_us = encode_us;
        self.nal_bytes_last = nal_bytes;
        self.encode_timestamps.push_back(now);
        self.encode_fps = Self::calc_fps(&mut self.encode_timestamps, now);

        self.encode_time_history
            .push_back(encode_us as f64 / 1000.0);
        if self.encode_time_history.len() > HISTORY_LEN {
            self.encode_time_history.pop_front();
        }
    }

    pub fn record_transport(&mut self, bytes: u64, fragments: u64) {
        let now = Instant::now();
        self.transport_packets_sent += 1;
        self.transport_fragments_sent += fragments;
        self.transport_bytes_sent += bytes;
        self.transport_timestamps.push_back(now);
        self.transport_fps = Self::calc_fps(&mut self.transport_timestamps, now);

        // Bandwidth tracking
        self.bandwidth_samples.push_back((now, bytes));
        let cutoff = now - std::time::Duration::from_secs(1);
        while self
            .bandwidth_samples
            .front()
            .is_some_and(|(t, _)| *t < cutoff)
        {
            self.bandwidth_samples.pop_front();
        }
        let total: u64 = self.bandwidth_samples.iter().map(|(_, b)| b).sum();
        self.bandwidth_bps = total as f64 * 8.0; // bits per second
    }

    pub fn uptime_secs(&self) -> f64 {
        self.start_time
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    fn calc_fps(timestamps: &mut VecDeque<Instant>, now: Instant) -> f64 {
        let cutoff = now - std::time::Duration::from_secs_f64(FPS_WINDOW_SECS);
        while timestamps.front().is_some_and(|t| *t < cutoff) {
            timestamps.pop_front();
        }
        if timestamps.len() >= 2 {
            let window = timestamps
                .back()
                .unwrap()
                .duration_since(*timestamps.front().unwrap());
            let secs = window.as_secs_f64();
            if secs > 0.0 {
                (timestamps.len() - 1) as f64 / secs
            } else {
                0.0
            }
        } else {
            0.0
        }
    }
}
