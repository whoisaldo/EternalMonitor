use std::collections::VecDeque;
use std::time::Instant;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

/// Global pipeline statistics shared between pipeline threads and the GUI.
pub static PIPELINE_STATS: Lazy<Mutex<PipelineStats>> =
    Lazy::new(|| Mutex::new(PipelineStats::new()));

const FPS_WINDOW_SECS: f64 = 1.0;
const HISTORY_LEN: usize = 300;

pub struct PipelineStats {
    pub listen_addr: String,

    // Capture
    pub capture_fps: f64,
    pub capture_resolution: (u32, u32),
    pub capture_frame_count: u64,
    pub gpu_name: String,
    /// Active capture source, e.g. `\\.\DISPLAY3 (2732x2048)`. Empty until capture starts.
    pub capture_display: String,
    capture_timestamps: VecDeque<Instant>,

    // Encoder
    pub encode_fps: f64,
    pub encode_time_us: u128,
    pub encode_frame_count: u64,
    pub nal_bytes_last: usize,
    pub bitrate_bps: u32,
    pub codec_name: String,
    /// True when a hardware encoder failed to open and the pipeline fell back to libx264
    /// (CPU) encoding. Surfaced as a warning banner in the GUI so a tester is never silently
    /// stuck on slow software encoding.
    pub using_software_fallback: bool,
    encode_timestamps: VecDeque<Instant>,

    // Transport
    pub transport_fps: f64,
    pub transport_bytes_sent: u64,
    pub transport_packets_sent: u64,
    pub transport_fragments_sent: u64,
    pub target_addr: String,
    pub latency_ms: f64,
    transport_timestamps: VecDeque<Instant>,

    // Bandwidth (bytes per second, rolling 1s window)
    bandwidth_samples: VecDeque<(Instant, u64)>,
    pub bandwidth_bps: f64,
    pub bandwidth_mbps: f64,

    // Encode time history for chart
    pub encode_time_history: VecDeque<f64>,

    // Pipeline state
    pub pipeline_running: bool,
    pub start_time: Option<Instant>,

    // mDNS
    pub mdns_active: bool,
}

impl Default for PipelineStats {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineStats {
    pub fn new() -> Self {
        Self {
            listen_addr: String::new(),

            capture_fps: 0.0,
            capture_resolution: (0, 0),
            capture_frame_count: 0,
            gpu_name: String::new(),
            capture_display: String::new(),
            capture_timestamps: VecDeque::with_capacity(128),

            encode_fps: 0.0,
            encode_time_us: 0,
            encode_frame_count: 0,
            nal_bytes_last: 0,
            bitrate_bps: 0,
            codec_name: String::new(),
            using_software_fallback: false,
            encode_timestamps: VecDeque::with_capacity(128),

            transport_fps: 0.0,
            transport_bytes_sent: 0,
            transport_packets_sent: 0,
            transport_fragments_sent: 0,
            target_addr: String::new(),
            latency_ms: 0.0,
            transport_timestamps: VecDeque::with_capacity(128),

            bandwidth_samples: VecDeque::with_capacity(256),
            bandwidth_bps: 0.0,
            bandwidth_mbps: 0.0,

            encode_time_history: VecDeque::with_capacity(HISTORY_LEN),

            pipeline_running: false,
            start_time: None,

            mdns_active: false,
        }
    }

    pub fn reset_for_restart(&mut self) {
        let gpu_name = self.gpu_name.clone();
        let capture_display = self.capture_display.clone();
        let listen_addr = self.listen_addr.clone();
        let target_addr = self.target_addr.clone();
        let bitrate_bps = self.bitrate_bps;
        let codec_name = self.codec_name.clone();
        let using_software_fallback = self.using_software_fallback;
        let mdns_active = self.mdns_active;

        *self = Self::new();
        self.gpu_name = gpu_name;
        self.capture_display = capture_display;
        self.listen_addr = listen_addr;
        self.target_addr = target_addr;
        self.bitrate_bps = bitrate_bps;
        self.codec_name = codec_name;
        self.using_software_fallback = using_software_fallback;
        self.mdns_active = mdns_active;
    }

    /// Clear transport counters and the bandwidth window without touching uptime,
    /// gpu_name, or the encode-time history. Called when an iPad re-handshakes on
    /// the same target so the GUI shows fresh session stats.
    pub fn reset_connection_stats(&mut self) {
        self.transport_fps = 0.0;
        self.transport_bytes_sent = 0;
        self.transport_packets_sent = 0;
        self.transport_fragments_sent = 0;
        self.transport_timestamps.clear();
        self.bandwidth_samples.clear();
        self.bandwidth_bps = 0.0;
        self.bandwidth_mbps = 0.0;
        self.latency_ms = 0.0;
    }

    pub fn mark_pipeline_started(&mut self) {
        self.pipeline_running = true;
        self.start_time = Some(Instant::now());
    }

    pub fn mark_pipeline_stopped(&mut self) {
        self.pipeline_running = false;
    }

    pub fn set_gpu_name(&mut self, gpu_name: String) {
        self.gpu_name = gpu_name;
    }

    pub fn set_capture_display(&mut self, capture_display: String) {
        self.capture_display = capture_display;
    }

    pub fn set_target_addr(&mut self, target_addr: String) {
        self.target_addr = target_addr;
    }

    pub fn set_listen_addr(&mut self, listen_addr: String) {
        self.listen_addr = listen_addr;
    }

    pub fn set_bitrate(&mut self, bitrate_bps: u32) {
        self.bitrate_bps = bitrate_bps;
    }

    pub fn set_codec_name(&mut self, codec_name: impl Into<String>) {
        self.codec_name = codec_name.into();
    }

    pub fn set_software_fallback(&mut self, using_software_fallback: bool) {
        self.using_software_fallback = using_software_fallback;
    }

    pub fn record_capture(&mut self, width: u32, height: u32) {
        let now = Instant::now();
        self.capture_frame_count += 1;
        self.capture_resolution = (width, height);
        self.capture_timestamps.push_back(now);
        self.capture_fps = Self::calc_fps(&mut self.capture_timestamps, now);
    }

    pub fn record_encode(&mut self, encode_us: u128, nal_bytes: usize, bitrate_bps: u32) {
        let now = Instant::now();
        self.encode_frame_count += 1;
        self.encode_time_us = encode_us;
        self.nal_bytes_last = nal_bytes;
        self.bitrate_bps = bitrate_bps;
        self.encode_timestamps.push_back(now);
        self.encode_fps = Self::calc_fps(&mut self.encode_timestamps, now);

        self.encode_time_history
            .push_back(encode_us as f64 / 1000.0);
        if self.encode_time_history.len() > HISTORY_LEN {
            self.encode_time_history.pop_front();
        }
    }

    pub fn record_transport(
        &mut self,
        bytes: u64,
        fragments: u64,
        latency_ms: f64,
        target_addr: String,
    ) {
        let now = Instant::now();
        self.transport_packets_sent += 1;
        self.transport_fragments_sent += fragments;
        self.transport_bytes_sent += bytes;
        self.transport_timestamps.push_back(now);
        self.transport_fps = Self::calc_fps(&mut self.transport_timestamps, now);
        self.latency_ms = latency_ms;
        self.target_addr = target_addr;

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
        self.bandwidth_bps = total as f64 * 8.0;
        self.bandwidth_mbps = self.bandwidth_bps / 1_000_000.0;
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

#[cfg(test)]
mod tests {
    use super::PipelineStats;

    #[test]
    fn reset_for_restart_preserves_runtime_configuration() {
        let mut stats = PipelineStats::new();
        stats.gpu_name = "GPU".to_string();
        stats.listen_addr = "192.168.1.10:9876".to_string();
        stats.target_addr = "10.0.0.1:9876".to_string();
        stats.bitrate_bps = 15_000_000;
        stats.codec_name = "H.264".to_string();
        stats.using_software_fallback = true;
        stats.mdns_active = true;
        stats.capture_frame_count = 10;
        stats.encode_frame_count = 8;
        stats.transport_packets_sent = 5;
        stats.encode_time_history.push_back(3.2);

        stats.reset_for_restart();

        assert_eq!(stats.gpu_name, "GPU");
        assert_eq!(stats.listen_addr, "192.168.1.10:9876");
        assert_eq!(stats.target_addr, "10.0.0.1:9876");
        assert_eq!(stats.bitrate_bps, 15_000_000);
        assert_eq!(stats.codec_name, "H.264");
        assert!(stats.using_software_fallback);
        assert!(stats.mdns_active);
        assert_eq!(stats.capture_frame_count, 0);
        assert_eq!(stats.encode_frame_count, 0);
        assert_eq!(stats.transport_packets_sent, 0);
        assert!(stats.encode_time_history.is_empty());
    }
}
