use std::ffi::OsStr;
use std::net::SocketAddr;
use std::os::windows::ffi::OsStrExt;

use eframe::egui;
use qrcode::{Color as QrModuleColor, QrCode};
use tracing::{info, warn};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPEN_CREATE_OPTIONS, REG_OPTION_NON_VOLATILE,
    REG_SZ,
};

use crate::capture::{enumerate_outputs, OutputInfo};
use crate::control::{CaptureTarget, GuiControl};
use crate::logging::{session_log_path, session_log_text};
use crate::settings::SettingsFile;
use crate::stats::PIPELINE_STATS;

// ── EternalMonitor // SIGNAL palette ──────────────────────────────────────────
// Broadcast-instrument control surface: void-black, hairline-framed panels,
// "transmit amber" as the brand/action color, "phosphor" mint for live/healthy
// signal, caution-yellow and fault-coral for states.
const BG: egui::Color32 = egui::Color32::from_rgb(6, 7, 8); // void
const SURFACE: egui::Color32 = egui::Color32::from_rgb(15, 16, 18); // panel
const SURFACE2: egui::Color32 = egui::Color32::from_rgb(23, 25, 28); // panel raised
const BORDER: egui::Color32 = egui::Color32::from_rgba_premultiplied(255, 255, 255, 18);

// Transmit amber — brand + primary action.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(255, 122, 26);
const ACCENT_BRIGHT: egui::Color32 = egui::Color32::from_rgb(255, 179, 92);
const ACCENT_FILL: egui::Color32 = egui::Color32::from_rgb(38, 24, 9); // amber tint on void
const ACCENT_BORDER: egui::Color32 = egui::Color32::from_rgb(92, 58, 22);

// Phosphor mint — live / healthy signal (formerly GREEN).
const GREEN: egui::Color32 = egui::Color32::from_rgb(62, 229, 166);
// Fault coral — error / loss (formerly RED).
const RED: egui::Color32 = egui::Color32::from_rgb(255, 77, 94);

const MUTED: egui::Color32 = egui::Color32::from_rgb(96, 102, 110);
const MUTED2: egui::Color32 = egui::Color32::from_rgb(150, 156, 164);
const TEXT: egui::Color32 = egui::Color32::from_rgb(242, 243, 245);
const CLEAR: egui::Color32 = egui::Color32::TRANSPARENT;
const SELECTION_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(45, 22, 5, 50);
const DANGER_BORDER: egui::Color32 = egui::Color32::from_rgb(92, 30, 36);
const DANGER_HOVER_FILL: egui::Color32 = egui::Color32::from_rgb(36, 14, 17);
const PILL_GREEN_FILL: egui::Color32 = egui::Color32::from_rgb(9, 32, 26);
const PILL_GREEN_BORDER: egui::Color32 = egui::Color32::from_rgb(20, 64, 52);
const PILL_RED_FILL: egui::Color32 = egui::Color32::from_rgb(36, 14, 17);
const PILL_RED_BORDER: egui::Color32 = egui::Color32::from_rgb(92, 30, 36);
const SPARKLINE_FILL_ALPHA: u8 = 22;

// Caution yellow — warnings (software fallback banner).
const WARN_AMBER: egui::Color32 = egui::Color32::from_rgb(255, 210, 63);
const WARN_FILL: egui::Color32 = egui::Color32::from_rgb(38, 33, 10);
const WARN_BORDER: egui::Color32 = egui::Color32::from_rgb(96, 80, 24);

const RUN_KEY_PATH: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE_NAME: PCWSTR = w!("EternalMonitor");

#[derive(PartialEq, Eq, Clone, Copy)]
enum AppTab {
    Stream,
    Performance,
    Settings,
}

struct StatsSnapshot {
    listen_addr: String,
    capture_fps: f64,
    capture_frame_count: u64,
    capture_resolution: (u32, u32),
    encode_fps: f64,
    encode_time_us: u128,
    encode_frame_count: u64,
    nal_bytes_last: usize,
    bitrate_bps: u32,
    codec_name: String,
    using_software_fallback: bool,
    gpu_name: String,
    capture_display: String,
    transport_fps: f64,
    transport_bytes_sent: u64,
    transport_packets_sent: u64,
    transport_fragments_sent: u64,
    target_addr: String,
    latency_ms: f64,
    bandwidth_mbps: f64,
    encode_time_history: Vec<f64>,
    pipeline_running: bool,
    uptime_secs: f64,
    mdns_active: bool,
    gpu_temp_c: Option<f64>,
}

impl StatsSnapshot {
    fn take() -> Self {
        let s = PIPELINE_STATS.lock();
        Self {
            listen_addr: s.listen_addr.clone(),
            capture_fps: s.capture_fps,
            capture_frame_count: s.capture_frame_count,
            capture_resolution: s.capture_resolution,
            encode_fps: s.encode_fps,
            encode_time_us: s.encode_time_us,
            encode_frame_count: s.encode_frame_count,
            nal_bytes_last: s.nal_bytes_last,
            bitrate_bps: s.bitrate_bps,
            codec_name: s.codec_name.clone(),
            using_software_fallback: s.using_software_fallback,
            gpu_name: s.gpu_name.clone(),
            capture_display: s.capture_display.clone(),
            transport_fps: s.transport_fps,
            transport_bytes_sent: s.transport_bytes_sent,
            transport_packets_sent: s.transport_packets_sent,
            transport_fragments_sent: s.transport_fragments_sent,
            target_addr: s.target_addr.clone(),
            latency_ms: s.latency_ms,
            bandwidth_mbps: s.bandwidth_mbps,
            encode_time_history: s.encode_time_history.iter().copied().collect(),
            pipeline_running: s.pipeline_running,
            uptime_secs: s.uptime_secs(),
            mdns_active: s.mdns_active,
            gpu_temp_c: s.gpu_temp_c,
        }
    }
}

const CAPTURE_AUTO_LABEL: &str = "Auto (primary)";

/// Combo label for an enumerated output, e.g. `DISPLAY3 · 2732×2048 · +2560,0 · primary`.
fn format_output_label(o: &OutputInfo) -> String {
    let short = o.device_name.rsplit('\\').next().unwrap_or(&o.device_name);
    let primary = if o.is_primary { " · primary" } else { "" };
    format!(
        "{} · {}×{} · +{},{}{}",
        short, o.width, o.height, o.left, o.top, primary
    )
}

const ENCODER_AUTO_LABEL: &str = "Auto";
const ENCODER_CHOICES: &[(&str, &str)] = &[
    (ENCODER_AUTO_LABEL, ""),
    ("NVENC", "h264_nvenc"),
    ("AMF", "h264_amf"),
    ("QSV", "h264_qsv"),
    ("x264", "libx264"),
];

pub struct AnalyzerApp {
    control: GuiControl,
    current_tab: AppTab,
    settings_bitrate_mbps: f32,
    settings_fps_target: u32,
    settings_target_ip: String,
    settings_target_error: Option<String>,
    settings_start_on_boot: bool,
    settings_encoder_choice: String, // display label, e.g. "Auto" or "NVENC"
    /// DXGI `DeviceName` of the chosen capture display; empty string means auto (primary).
    settings_capture_display: String,
    /// Cached enumerated outputs for the capture-display picker; refreshed on demand.
    available_outputs: Vec<OutputInfo>,
    show_qr_modal: bool,
    qr_cache: Option<(String, QrCode)>,
}

impl AnalyzerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, control: GuiControl) -> Self {
        // Load persisted settings first; values fall back to the live runtime state when the
        // file is missing or unreadable.
        let persisted = SettingsFile::load();

        let runtime_bitrate_mbps = control
            .shared
            .bitrate_bps
            .load(std::sync::atomic::Ordering::SeqCst) as f32
            / 1_000_000.0;
        let bitrate_mbps = if persisted.bitrate_mbps > 0.0 {
            control.shared.bitrate_bps.store(
                (persisted.bitrate_mbps * 1_000_000.0).round() as u32,
                std::sync::atomic::Ordering::SeqCst,
            );
            PIPELINE_STATS
                .lock()
                .set_bitrate((persisted.bitrate_mbps * 1_000_000.0).round() as u32);
            persisted.bitrate_mbps
        } else {
            runtime_bitrate_mbps
        };

        let fps_target = if persisted.target_fps == 30 || persisted.target_fps == 60 {
            persisted.target_fps
        } else {
            control
                .shared
                .target_fps
                .load(std::sync::atomic::Ordering::SeqCst)
        };
        control
            .shared
            .target_fps
            .store(fps_target, std::sync::atomic::Ordering::SeqCst);

        let settings_target_ip = if let Some(ip) = persisted.target_ip.clone() {
            if let Ok(addr) = ip.parse::<SocketAddr>() {
                *control.shared.target_addr.lock() = addr;
                PIPELINE_STATS.lock().set_target_addr(addr.to_string());
            }
            ip
        } else {
            let target_addr = *control.shared.target_addr.lock();
            if target_addr.ip().is_unspecified() || target_addr.port() == 0 {
                String::new()
            } else {
                target_addr.to_string()
            }
        };

        let encoder_choice = if let Some(name) = persisted.encoder_override.as_deref() {
            *control.shared.encoder_override.lock() = Some(name.to_string());
            ENCODER_CHOICES
                .iter()
                .find(|(_, ffmpeg)| *ffmpeg == name)
                .map(|(label, _)| label.to_string())
                .unwrap_or_else(|| ENCODER_AUTO_LABEL.to_string())
        } else {
            ENCODER_AUTO_LABEL.to_string()
        };

        // Apply the persisted capture display into SharedControl so the next stream restart
        // honors it (same model as encoder_override — the first pipeline run started before
        // the GUI loaded settings).
        let settings_capture_display = match persisted.capture_display.as_deref() {
            Some(name) if !name.is_empty() => {
                *control.shared.capture_target.lock() = CaptureTarget::Output(name.to_string());
                name.to_string()
            }
            _ => {
                *control.shared.capture_target.lock() = CaptureTarget::PrimaryAuto;
                String::new()
            }
        };
        let available_outputs = enumerate_outputs();

        let start_on_boot = if persisted.start_on_boot != read_startup_registry() {
            // Persisted state disagrees with the registry — trust the registry as ground truth.
            read_startup_registry()
        } else {
            persisted.start_on_boot
        };

        Self {
            control,
            current_tab: AppTab::Stream,
            settings_bitrate_mbps: bitrate_mbps,
            settings_fps_target: fps_target,
            settings_target_ip,
            settings_target_error: None,
            settings_start_on_boot: start_on_boot,
            settings_encoder_choice: encoder_choice,
            settings_capture_display,
            available_outputs,
            show_qr_modal: false,
            qr_cache: None,
        }
    }

    fn apply_target_addr(&mut self) {
        match self.settings_target_ip.trim().parse::<SocketAddr>() {
            Ok(target_addr) => {
                *self.control.shared.target_addr.lock() = target_addr;
                PIPELINE_STATS.lock().set_target_addr(target_addr.to_string());
                self.settings_target_error = None;
                info!(target = %target_addr, "Transport target updated from GUI");
                self.persist_settings();
            }
            Err(error) => {
                self.settings_target_error = Some("Enter host:port".to_string());
                warn!(error = %error, target = %self.settings_target_ip, "Invalid target address");
            }
        }
    }

    fn persist_settings(&self) {
        let encoder_override = ENCODER_CHOICES
            .iter()
            .find(|(label, _)| *label == self.settings_encoder_choice)
            .and_then(|(_, ffmpeg)| {
                if ffmpeg.is_empty() {
                    None
                } else {
                    Some((*ffmpeg).to_string())
                }
            });
        let file = SettingsFile {
            bitrate_mbps: self.settings_bitrate_mbps,
            target_fps: self.settings_fps_target,
            target_ip: if self.settings_target_ip.trim().is_empty() {
                None
            } else {
                Some(self.settings_target_ip.trim().to_string())
            },
            encoder_override,
            capture_display: if self.settings_capture_display.is_empty() {
                None
            } else {
                Some(self.settings_capture_display.clone())
            },
            start_on_boot: self.settings_start_on_boot,
        };
        file.save();
    }
}

impl eframe::App for AnalyzerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));

        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = SURFACE;
        visuals.widgets.noninteractive.bg_fill = SURFACE;
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
        visuals.widgets.inactive.bg_fill = SURFACE2;
        visuals.widgets.inactive.weak_bg_fill = CLEAR;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, MUTED);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, MUTED);
        visuals.widgets.hovered.bg_fill = SURFACE2;
        visuals.widgets.hovered.weak_bg_fill = CLEAR;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, MUTED2);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, TEXT);
        visuals.widgets.active.bg_fill = SURFACE2;
        visuals.widgets.active.weak_bg_fill = CLEAR;
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, TEXT);
        visuals.selection.bg_fill = SELECTION_BG;
        visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
        // Instrument-grade: tight corners, hairline focus ring in amber.
        let r = egui::CornerRadius::same(4);
        visuals.widgets.noninteractive.corner_radius = r;
        visuals.widgets.inactive.corner_radius = r;
        visuals.widgets.hovered.corner_radius = r;
        visuals.widgets.active.corner_radius = r;
        visuals.widgets.open.corner_radius = r;
        ctx.set_visuals(visuals);

        let snap = StatsSnapshot::take();
        if self.settings_target_ip.is_empty()
            && !snap.target_addr.is_empty()
            && snap.target_addr != "0.0.0.0:9876"
        {
            self.settings_target_ip = snap.target_addr.clone();
        }

        self.draw_sidebar(ctx, &snap);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.current_tab {
                AppTab::Stream => self.draw_stream_tab(ui, &snap),
                AppTab::Performance => self.draw_performance_tab(ui, &snap),
                AppTab::Settings => self.draw_settings_tab(ui),
            });
        });

        if self.show_qr_modal {
            self.draw_qr_modal(ctx, &snap);
        }
    }
}

impl AnalyzerApp {
    fn draw_sidebar(&mut self, ctx: &egui::Context, snap: &StatsSnapshot) {
        egui::SidePanel::left("sidebar")
            .exact_width(200.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(egui::Stroke::new(1.0, BORDER))
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ctx, |ui| {
                logo_widget(ui);
                ui.add_space(24.0);

                self.nav_item(ui, "Stream", AppTab::Stream);
                self.nav_item(ui, "Performance", AppTab::Performance);
                self.nav_item(ui, "Settings", AppTab::Settings);

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(4.0);
                    status_pill(ui, ctx, snap.pipeline_running, &snap.target_addr);
                });
            });
    }

    fn nav_item(&mut self, ui: &mut egui::Ui, label: &str, tab: AppTab) {
        let active = self.current_tab == tab;
        let (bg, text_color) = if active {
            (SURFACE2, ACCENT)
        } else {
            (CLEAR, MUTED2)
        };

        let btn = egui::Button::new(
            egui::RichText::new(label.to_uppercase())
                .color(text_color)
                .size(12.0)
                .monospace()
                .strong(),
        )
        .fill(bg)
        .stroke(egui::Stroke::NONE)
        .corner_radius(4.0)
        .min_size(egui::vec2(ui.available_width(), 32.0));

        let resp = ui.add(btn);
        if active {
            // Amber registration bar on the left edge of the active tab.
            let bar = egui::Rect::from_min_size(
                resp.rect.left_top() + egui::vec2(0.0, 4.0),
                egui::vec2(2.5, resp.rect.height() - 8.0),
            );
            ui.painter().rect_filled(bar, 0.0, ACCENT);
        }
        if resp.clicked() {
            self.current_tab = tab;
        }
        ui.add_space(4.0);
    }

    fn draw_stream_tab(&mut self, ui: &mut egui::Ui, snap: &StatsSnapshot) {
        ui.add_space(8.0);
        if snap.using_software_fallback {
            software_fallback_banner(ui);
            ui.add_space(12.0);
        }

        // ── Hero: how the iPad connects ──────────────────────────────────────
        hero_module(ui, |ui| {
            ui.set_width(ui.available_width());
            section_header(ui, "Connect your iPad");
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(value_or_unknown(&snap.listen_addr))
                        .color(TEXT)
                        .monospace()
                        .strong()
                        .size(22.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if amber_button(ui, "QR code").clicked() {
                        self.show_qr_modal = true;
                    }
                    if ghost_button(ui, "Copy IP", true).clicked() && !snap.listen_addr.is_empty() {
                        ui.ctx().copy_text(snap.listen_addr.clone());
                    }
                });
            });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Open EternalMonitor on the iPad — scan the QR with its camera, or type this address into the app.",
                )
                .color(MUTED2)
                .size(11.0),
            );
        });

        ui.add_space(12.0);

        // ── Live readouts ────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            readout_card(ui, "Frame rate", &format!("{:.0}", snap.capture_fps), "fps", ACCENT);
            readout_card(
                ui,
                "Encode",
                &format!("{:.1}", snap.encode_time_us as f64 / 1000.0),
                "ms",
                TEXT,
            );
            readout_card(ui, "Latency", &format!("{:.0}", snap.latency_ms), "ms", GREEN);
            readout_card(ui, "Bitrate", &format!("{:.1}", snap.bandwidth_mbps), "mbps", ACCENT);
        });

        ui.add_space(12.0);

        ui.columns(2, |cols| {
            card_frame().show(&mut cols[0], |ui| {
                ui.set_width(ui.available_width());
                section_header(ui, "Encoder");
                stat_row(ui, "GPU", value_or_unknown(&snap.gpu_name));
                stat_row(ui, "Capture display", value_or_unknown(&snap.capture_display));
                stat_row(ui, "Codec", value_or_unknown(&snap.codec_name));
                stat_row(
                    ui,
                    "Resolution",
                    &format!(
                        "{}x{}",
                        snap.capture_resolution.0, snap.capture_resolution.1
                    ),
                );
                stat_row(ui, "Bitrate", &format_bitrate(snap.bitrate_bps));
                stat_row(ui, "Frames", &snap.encode_frame_count.to_string());
                stat_row(ui, "NAL size", &format_bytes(snap.nal_bytes_last as u64));
            });

            card_frame().show(&mut cols[1], |ui| {
                ui.set_width(ui.available_width());
                section_header(ui, "Encode scope · ms");
                draw_sparkline(ui, &snap.encode_time_history, 132.0, ACCENT);
            });
        });

        ui.add_space(12.0);

        ui.horizontal(|ui| {
            if amber_button(ui, "Restart stream").clicked() {
                self.control.request_restart();
            }
            let has_logs = session_log_text().is_some();
            let copy_logs_button = ghost_button(ui, "Copy logs", has_logs);
            if copy_logs_button.clicked() {
                if let Some(log_text) = session_log_text() {
                    ui.ctx().copy_text(log_text);
                }
            }
            if has_logs {
                copy_logs_button.on_hover_text(format!(
                    "Copies the full session log from {}",
                    session_log_path().display()
                ));
            } else {
                copy_logs_button.on_hover_text("No recent log lines have been captured yet.");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if danger_button(ui, "Stop").clicked() {
                    self.control.shared.stop();
                }
            });
        });
    }

    fn draw_performance_tab(&self, ui: &mut egui::Ui, snap: &StatsSnapshot) {
        ui.add_space(8.0);

        // Full-width encode time scope, auto-scaled.
        module_with_ticks(ui, ACCENT, |ui| {
            ui.set_width(ui.available_width());
            section_header(ui, "Encode scope · ms");
            draw_sparkline(ui, &snap.encode_time_history, 188.0, ACCENT);
        });

        ui.add_space(12.0);

        // Capture / Encode / Transport FPS · Bandwidth
        ui.horizontal(|ui| {
            readout_card(ui, "Capture", &format!("{:.0}", snap.capture_fps), "fps", ACCENT);
            readout_card(ui, "Encode", &format!("{:.0}", snap.encode_fps), "fps", ACCENT);
            readout_card(ui, "Transport", &format!("{:.0}", snap.transport_fps), "fps", ACCENT);
            readout_card(ui, "Bandwidth", &format!("{:.1}", snap.bandwidth_mbps), "mbps", GREEN);
        });

        ui.add_space(12.0);

        // Cumulative session counters
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_header(ui, "Session totals");
            stat_row(
                ui,
                "Frames sent",
                &snap.encode_frame_count.to_string(),
            );
            stat_row(
                ui,
                "Bytes sent",
                &format_bytes(snap.transport_bytes_sent),
            );
            stat_row(
                ui,
                "Packets sent",
                &snap.transport_packets_sent.to_string(),
            );
            stat_row(
                ui,
                "Fragments sent",
                &snap.transport_fragments_sent.to_string(),
            );
            stat_row(ui, "Frames captured", &snap.capture_frame_count.to_string());
            stat_row(ui, "Uptime", &format_uptime(snap.uptime_secs));
            stat_row(ui, "Target", value_or_unknown(&snap.target_addr));
            stat_row(ui, "mDNS", if snap.mdns_active { "Active" } else { "Inactive" });
            let gpu_temp = snap
                .gpu_temp_c
                .map(|t| format!("{:.1} °C", t))
                .unwrap_or_else(|| "unavailable".to_string());
            stat_row(ui, "GPU temp", &gpu_temp);
        });
    }

    fn draw_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        section_header(ui, "Settings");

        card_frame().show(ui, |ui| {
            // --- Bitrate slider ---------------------------------------------------
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!(
                    "Bitrate: {:.0} Mbps",
                    self.settings_bitrate_mbps
                ))
                .color(TEXT)
                .size(13.0));
            });
            let slider = egui::Slider::new(&mut self.settings_bitrate_mbps, 1.0..=50.0)
                .show_value(false);
            if ui.add(slider).changed() {
                let bitrate_bps = (self.settings_bitrate_mbps * 1_000_000.0).round() as u32;
                self.control
                    .shared
                    .bitrate_bps
                    .store(bitrate_bps, std::sync::atomic::Ordering::SeqCst);
                PIPELINE_STATS.lock().set_bitrate(bitrate_bps);
                self.persist_settings();
            }

            ui.add_space(12.0);

            // --- FPS target segmented control ------------------------------------
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("FPS target").color(TEXT).size(13.0));
                ui.add_space(8.0);
                let prev = self.settings_fps_target;
                if ui
                    .selectable_label(self.settings_fps_target == 30, "30")
                    .clicked()
                {
                    self.settings_fps_target = 30;
                }
                if ui
                    .selectable_label(self.settings_fps_target == 60, "60")
                    .clicked()
                {
                    self.settings_fps_target = 60;
                }
                if self.settings_fps_target != prev {
                    self.control
                        .shared
                        .target_fps
                        .store(self.settings_fps_target, std::sync::atomic::Ordering::SeqCst);
                    info!(
                        target_fps = self.settings_fps_target,
                        "Capture target FPS updated from GUI"
                    );
                    self.persist_settings();
                }
            });

            ui.add_space(12.0);

            // --- Target IP --------------------------------------------------------
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Target IP").color(TEXT).size(13.0));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.settings_target_ip).desired_width(220.0),
                );
                let enter_pressed =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if amber_button(ui, "Apply").clicked() || enter_pressed {
                    self.apply_target_addr();
                }
            });
            if let Some(error) = &self.settings_target_error {
                ui.label(egui::RichText::new(error).color(RED).size(11.0));
            }

            ui.add_space(12.0);

            // --- Encoder override dropdown ---------------------------------------
            let detected = PIPELINE_STATS.lock().codec_name.clone();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "Encoder: {} (detected: {})",
                        self.settings_encoder_choice,
                        if detected.is_empty() {
                            "Unknown"
                        } else {
                            &detected
                        }
                    ))
                    .color(TEXT)
                    .size(13.0),
                );
            });
            let prev_choice = self.settings_encoder_choice.clone();
            egui::ComboBox::from_id_salt("encoder_override_combo")
                .selected_text(&self.settings_encoder_choice)
                .show_ui(ui, |ui| {
                    for (label, _) in ENCODER_CHOICES {
                        if ui
                            .selectable_label(self.settings_encoder_choice == *label, *label)
                            .clicked()
                        {
                            self.settings_encoder_choice = (*label).to_string();
                        }
                    }
                });
            if self.settings_encoder_choice != prev_choice {
                let ffmpeg_name = ENCODER_CHOICES
                    .iter()
                    .find(|(label, _)| *label == self.settings_encoder_choice)
                    .map(|(_, ffmpeg)| (*ffmpeg).to_string());
                *self.control.shared.encoder_override.lock() = ffmpeg_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                info!(
                    encoder = self.settings_encoder_choice,
                    "Encoder override set — takes effect on next stream restart"
                );
                self.persist_settings();
            }

            ui.add_space(12.0);

            // --- Capture display picker ------------------------------------------
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Capture display").color(TEXT).size(13.0));
                if ghost_button(ui, "Refresh", true).clicked() {
                    self.available_outputs = enumerate_outputs();
                    info!(
                        count = self.available_outputs.len(),
                        "Re-enumerated display outputs"
                    );
                }
            });
            let outputs = self.available_outputs.clone();
            let prev_display = self.settings_capture_display.clone();
            let selected_text = if self.settings_capture_display.is_empty() {
                CAPTURE_AUTO_LABEL.to_string()
            } else if let Some(o) = outputs
                .iter()
                .find(|o| o.device_name == self.settings_capture_display)
            {
                format_output_label(o)
            } else {
                format!("{} (not connected)", self.settings_capture_display)
            };
            egui::ComboBox::from_id_salt("capture_display_combo")
                .selected_text(selected_text)
                .width(280.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            self.settings_capture_display.is_empty(),
                            CAPTURE_AUTO_LABEL,
                        )
                        .clicked()
                    {
                        self.settings_capture_display = String::new();
                    }
                    for o in &outputs {
                        let selected = self.settings_capture_display == o.device_name;
                        if ui.selectable_label(selected, format_output_label(o)).clicked() {
                            self.settings_capture_display = o.device_name.clone();
                        }
                    }
                });
            if self.settings_capture_display != prev_display {
                *self.control.shared.capture_target.lock() =
                    if self.settings_capture_display.is_empty() {
                        CaptureTarget::PrimaryAuto
                    } else {
                        CaptureTarget::Output(self.settings_capture_display.clone())
                    };
                info!(
                    display = %self.settings_capture_display,
                    "Capture display set — takes effect on next stream restart"
                );
                self.persist_settings();
            }
            ui.label(
                egui::RichText::new("Applies on next Restart stream")
                    .color(MUTED)
                    .size(11.0),
            );

            ui.add_space(12.0);

            // --- Start on Windows startup ----------------------------------------
            let prev_boot = self.settings_start_on_boot;
            ui.checkbox(
                &mut self.settings_start_on_boot,
                egui::RichText::new("Start on Windows startup").color(TEXT).size(13.0),
            );
            if self.settings_start_on_boot != prev_boot {
                if let Err(error) = set_startup_registry(self.settings_start_on_boot) {
                    self.settings_start_on_boot = prev_boot;
                    self.settings_target_error = Some(error);
                } else {
                    self.settings_target_error = None;
                    self.persist_settings();
                }
            }
        });
    }
}

impl AnalyzerApp {
    fn draw_qr_modal(&mut self, ctx: &egui::Context, snap: &StatsSnapshot) {
        let listen_addr = if snap.listen_addr.is_empty() {
            self.control.shared.target_addr.lock().to_string()
        } else {
            snap.listen_addr.clone()
        };
        let url = format!("eternaldisplay://{}", listen_addr);

        // Cache the encoded QR matrix until the URL changes.
        if self
            .qr_cache
            .as_ref()
            .map(|(cached_url, _)| cached_url != &url)
            .unwrap_or(true)
        {
            match QrCode::new(url.as_bytes()) {
                Ok(code) => self.qr_cache = Some((url.clone(), code)),
                Err(error) => {
                    warn!(error = %error, url = %url, "Failed to encode QR code");
                    self.qr_cache = None;
                }
            }
        }

        let qr = self.qr_cache.as_ref().map(|(_, c)| c);
        let mut should_close = false;

        egui::Window::new("QR Code")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Scan this QR with the iPad camera to connect.")
                        .color(TEXT)
                        .size(14.0),
                );
                ui.add_space(8.0);

                let canvas_size = 360.0_f32;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(canvas_size, canvas_size),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 6.0, egui::Color32::WHITE);
                // Amber viewfinder framing — reads as a scan target.
                viewfinder_marks(&painter, rect, ACCENT, 18.0);

                if let Some(code) = qr {
                    let width = code.width();
                    let modules = code.to_colors();
                    let quiet_zone = 4.0_f32;
                    let module_size = (canvas_size - 2.0 * quiet_zone) / width as f32;
                    let origin = egui::pos2(rect.left() + quiet_zone, rect.top() + quiet_zone);

                    for y in 0..width {
                        for x in 0..width {
                            if matches!(modules[y * width + x], QrModuleColor::Dark) {
                                let cell = egui::Rect::from_min_size(
                                    egui::pos2(
                                        origin.x + x as f32 * module_size,
                                        origin.y + y as f32 * module_size,
                                    ),
                                    egui::vec2(module_size, module_size),
                                );
                                painter.rect_filled(cell, 0.0, egui::Color32::BLACK);
                            }
                        }
                    }
                } else {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "QR encoding failed",
                        egui::FontId::monospace(14.0),
                        RED,
                    );
                }

                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(&url)
                        .color(MUTED2)
                        .monospace()
                        .size(13.0),
                );
                ui.add_space(8.0);
                if amber_button(ui, "Close").clicked() {
                    should_close = true;
                }
            });

        if should_close {
            self.show_qr_modal = false;
        }
    }
}

/// Amber warning banner shown on the Stream tab when the pipeline fell back to CPU
/// (libx264) encoding because the hardware encoder failed to open. Keeps a tester from
/// silently running a hot, high-latency software encode and blaming the app.
fn software_fallback_banner(ui: &mut egui::Ui) {
    egui::Frame::new()
        .fill(WARN_FILL)
        .stroke(egui::Stroke::new(1.0, WARN_BORDER))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("⚠ Hardware encoder unavailable — encoding on CPU (libx264)")
                    .color(WARN_AMBER)
                    .strong()
                    .size(13.0),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "Expect higher latency and heavy CPU load. Update your GPU drivers \
                     (NVIDIA/AMD/Intel) and restart the stream to use hardware encoding.",
                )
                .color(TEXT)
                .size(11.0),
            );
        });
}

fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(12))
}

/// Draw viewfinder / registration corner ticks just inside `rect` — the recurring
/// "this is a monitor" motif. `len` is the arm length of each L-shaped tick.
fn viewfinder_marks(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32, len: f32) {
    let stroke = egui::Stroke::new(1.0, color);
    let inset = 3.0;
    let l = rect.left() + inset;
    let r = rect.right() - inset;
    let t = rect.top() + inset;
    let b = rect.bottom() - inset;
    // top-left
    painter.line_segment([egui::pos2(l, t), egui::pos2(l + len, t)], stroke);
    painter.line_segment([egui::pos2(l, t), egui::pos2(l, t + len)], stroke);
    // top-right
    painter.line_segment([egui::pos2(r, t), egui::pos2(r - len, t)], stroke);
    painter.line_segment([egui::pos2(r, t), egui::pos2(r, t + len)], stroke);
    // bottom-left
    painter.line_segment([egui::pos2(l, b), egui::pos2(l + len, b)], stroke);
    painter.line_segment([egui::pos2(l, b), egui::pos2(l, b - len)], stroke);
    // bottom-right
    painter.line_segment([egui::pos2(r, b), egui::pos2(r - len, b)], stroke);
    painter.line_segment([egui::pos2(r, b), egui::pos2(r, b - len)], stroke);
}

/// A panel module that paints viewfinder ticks in its corners after laying out `content`.
fn module_with_ticks(
    ui: &mut egui::Ui,
    tick_color: egui::Color32,
    content: impl FnOnce(&mut egui::Ui),
) {
    let resp = card_frame().show(ui, content).response;
    viewfinder_marks(&ui.painter_at(resp.rect), resp.rect, tick_color, 9.0);
}

/// Amber-tinted hero module with brighter ticks — used for the primary "connect"
/// surface so it reads as the focal point of the console.
fn hero_module(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    let resp = egui::Frame::new()
        .fill(ACCENT_FILL)
        .stroke(egui::Stroke::new(1.0, ACCENT_BORDER))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(14))
        .show(ui, content)
        .response;
    viewfinder_marks(&ui.painter_at(resp.rect), resp.rect, ACCENT, 12.0);
}

/// Solid amber primary action button (dark label).
fn amber_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text.to_uppercase())
                .color(BG)
                .size(12.0)
                .monospace()
                .strong(),
        )
        .fill(ACCENT)
        .corner_radius(4.0)
        .min_size(egui::vec2(0.0, 30.0)),
    )
}

/// Hairline "ghost" button — quiet secondary action.
fn ghost_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            egui::RichText::new(text.to_uppercase())
                .color(MUTED2)
                .size(12.0)
                .monospace(),
        )
        .fill(SURFACE2)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(4.0)
        .min_size(egui::vec2(0.0, 30.0)),
    )
}

/// Fault-colored destructive button (outline that fills coral on hover).
fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.widgets.inactive.weak_bg_fill = CLEAR;
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, DANGER_BORDER);
        v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, RED);
        v.widgets.hovered.weak_bg_fill = DANGER_HOVER_FILL;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, RED);
        v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, RED);
        ui.add(
            egui::Button::new(
                egui::RichText::new(text.to_uppercase()).size(12.0).monospace().strong(),
            )
            .corner_radius(4.0)
            .min_size(egui::vec2(0.0, 30.0)),
        )
    })
    .inner
}


fn logo_widget(ui: &mut egui::Ui) {
    // Signal-level mark: three ascending amber bars (like a transmit meter).
    let start = ui.cursor().min;
    let bar_w = 6.0;
    let gap = 4.0;
    let heights = [9.0_f32, 15.0, 22.0];
    let base_y = start.y + 22.0;
    for (i, h) in heights.iter().enumerate() {
        let x = start.x + i as f32 * (bar_w + gap);
        let rect = egui::Rect::from_min_max(egui::pos2(x, base_y - h), egui::pos2(x + bar_w, base_y));
        ui.painter().rect_filled(rect, 1.0, ACCENT);
    }
    ui.add_space(28.0);
    ui.label(
        egui::RichText::new("ETERNAL")
            .color(TEXT)
            .size(18.0)
            .strong()
            .line_height(Some(19.0)),
    );
    ui.label(
        egui::RichText::new("MONITOR")
            .color(ACCENT)
            .size(18.0)
            .strong()
            .line_height(Some(19.0)),
    );
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("// SIGNAL")
            .color(MUTED)
            .size(10.0)
            .monospace(),
    );
}

fn status_pill(ui: &mut egui::Ui, ctx: &egui::Context, running: bool, target_addr: &str) {
    let (fill, border) = if running {
        (PILL_GREEN_FILL, PILL_GREEN_BORDER)
    } else {
        (PILL_RED_FILL, PILL_RED_BORDER)
    };

    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let dot_color = if running {
                    ui.ctx().request_repaint();
                    let t = (ctx.input(|i| i.time) % 1.4) as f32 / 1.4;
                    let pulse = 0.5 - 0.5 * (t * std::f32::consts::TAU).cos();
                    let a = (110.0 + 145.0 * pulse) as u8;
                    egui::Color32::from_rgba_unmultiplied(GREEN.r(), GREEN.g(), GREEN.b(), a)
                } else {
                    RED
                };
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, dot_color);
                ui.add_space(2.0);

                let label = if running { "ON AIR" } else { "OFFLINE" };
                let label_color = if running { GREEN } else { RED };
                ui.label(
                    egui::RichText::new(label)
                        .color(label_color)
                        .size(11.0)
                        .strong(),
                );
            });
            if !target_addr.is_empty() && target_addr != "0.0.0.0:9876" {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!("→ {target_addr}"))
                        .color(MUTED2)
                        .size(10.0)
                        .monospace(),
                );
            } else if running {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("waiting for iPad")
                        .color(MUTED)
                        .size(10.0)
                        .monospace(),
                );
            }
        });
}

/// Equipment-style readout: a big monospace value with a small unit subscript,
/// inside a hairline module. `accent` colors the value; the unit stays muted.
fn readout_card(ui: &mut egui::Ui, label: &str, value: &str, unit: &str, accent: egui::Color32) {
    card_frame().show(ui, |ui| {
        ui.set_min_width(96.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(label.to_uppercase())
                    .color(MUTED)
                    .size(10.0)
                    .monospace(),
            );
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.label(
                    egui::RichText::new(value)
                        .color(accent)
                        .size(26.0)
                        .strong()
                        .monospace(),
                );
                if !unit.is_empty() {
                    ui.label(
                        egui::RichText::new(unit)
                            .color(MUTED2)
                            .size(11.0)
                            .monospace(),
                    );
                }
            });
        });
    });
}

fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (tick, _) = ui.allocate_exact_size(egui::vec2(3.0, 11.0), egui::Sense::hover());
        ui.painter().rect_filled(tick, 0.0, ACCENT);
        ui.label(
            egui::RichText::new(text.to_uppercase())
                .color(MUTED2)
                .size(10.0)
                .strong()
                .monospace(),
        );
    });
    ui.add_space(6.0);
}

fn stat_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(MUTED).size(11.0).monospace());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).color(TEXT).size(11.0).monospace());
        });
    });
}

/// Oscilloscope-style telemetry trace: a gridded screen with a glowing line and
/// peak/last readouts in the corner. Replaces the old flat sparkline.
fn draw_sparkline(ui: &mut egui::Ui, data: &[f64], height: f32, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);

    // Screen
    painter.rect_filled(rect, 4.0, BG);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );

    // Grid graticule
    let grid = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10));
    let cols = 8;
    let rows = 4;
    for c in 1..cols {
        let x = rect.left() + rect.width() * c as f32 / cols as f32;
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], grid);
    }
    for r in 1..rows {
        let y = rect.top() + rect.height() * r as f32 / rows as f32;
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], grid);
    }

    if data.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "ACQUIRING SIGNAL…",
            egui::FontId::monospace(12.0),
            MUTED,
        );
        return;
    }

    let min_val = data.iter().cloned().fold(f64::MAX, f64::min);
    let max_val = data.iter().cloned().fold(f64::MIN, f64::max);
    let range = (max_val - min_val).max(0.001);
    let pad = 8.0;

    let points: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = rect.left() + (i as f32 / (data.len() - 1) as f32) * rect.width();
            let y = rect.bottom() - pad - (((v - min_val) / range) as f32 * (height - 2.0 * pad));
            egui::pos2(x, y)
        })
        .collect();

    // Area fill under the trace
    let fill_color =
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), SPARKLINE_FILL_ALPHA);
    let baseline = rect.bottom();
    let mut fill_points = vec![egui::pos2(points[0].x, baseline)];
    fill_points.extend_from_slice(&points);
    fill_points.push(egui::pos2(points[points.len() - 1].x, baseline));
    painter.add(egui::Shape::convex_polygon(
        fill_points,
        fill_color,
        egui::Stroke::NONE,
    ));

    // Glow underlay + crisp trace on top
    let glow = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 60);
    painter.add(egui::Shape::line(points.clone(), egui::Stroke::new(4.0, glow)));
    painter.add(egui::Shape::line(points.clone(), egui::Stroke::new(1.5, color)));

    // Bright dot at the latest sample
    if let Some(last) = points.last() {
        painter.circle_filled(*last, 2.5, ACCENT_BRIGHT);
    }

    // Peak / last readout
    painter.text(
        egui::pos2(rect.right() - 6.0, rect.top() + 5.0),
        egui::Align2::RIGHT_TOP,
        format!("pk {max_val:.1}  ·  now {:.1}", data.last().copied().unwrap_or(0.0)),
        egui::FontId::monospace(10.0),
        MUTED2,
    );
}

fn format_bytes(bytes: u64) -> String {
    if bytes > 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes > 1_000_000 {
        format!("{:.2} MB", bytes as f64 / 1_000_000.0)
    } else if bytes > 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_bitrate(bitrate_bps: u32) -> String {
    format!("{:.1} Mbps", bitrate_bps as f64 / 1_000_000.0)
}

fn format_uptime(uptime_secs: f64) -> String {
    let mins = (uptime_secs / 60.0).floor() as u64;
    let secs = (uptime_secs % 60.0).floor() as u64;
    format!("{mins}m {secs}s")
}

fn value_or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "Unknown"
    } else {
        value
    }
}

pub(crate) fn detect_local_ip(listen_port: u16) -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| format!("{}:{}", a.ip(), listen_port))
        .unwrap_or_else(|_| format!("unknown:{listen_port}"))
}

fn read_startup_registry() -> bool {
    match open_run_key(KEY_READ) {
        Ok(key) => {
            let result = unsafe {
                RegQueryValueExW(
                    key.0,
                    RUN_VALUE_NAME,
                    None,
                    None,
                    None,
                    Some(&mut 0u32),
                )
            };
            result == ERROR_SUCCESS
        }
        Err(_) => false,
    }
}

fn set_startup_registry(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe_path = std::env::current_exe().map_err(|error| error.to_string())?;
        let key = create_run_key()?;
        let exe_path = utf16_bytes(exe_path.as_os_str());
        let status = unsafe { RegSetValueExW(key.0, RUN_VALUE_NAME, 0, REG_SZ, Some(&exe_path)) };
        if status == ERROR_SUCCESS {
            info!("Startup registry updated");
            Ok(())
        } else {
            Err(format!("Failed to set startup entry: {:?}", status))
        }
    } else {
        let key = open_run_key(KEY_SET_VALUE)?;
        let status = unsafe { RegDeleteValueW(key.0, RUN_VALUE_NAME) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            info!("Startup registry updated");
            Ok(())
        } else {
            Err(format!("Failed to remove startup entry: {:?}", status))
        }
    }
}

fn create_run_key() -> Result<OwnedRegKey, String> {
    let mut key = HKEY::default();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY_PATH,
            0,
            PCWSTR::null(),
            REG_OPEN_CREATE_OPTIONS(REG_OPTION_NON_VOLATILE.0),
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(OwnedRegKey(key))
    } else {
        Err(format!("Failed to open Run key: {:?}", status))
    }
}

fn open_run_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<OwnedRegKey, String> {
    let mut key = HKEY::default();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY_PATH, 0, access, &mut key) };
    if status == ERROR_SUCCESS {
        Ok(OwnedRegKey(key))
    } else {
        Err(format!("Failed to open Run key: {:?}", status))
    }
}

fn utf16_bytes(value: &OsStr) -> Vec<u8> {
    let wide: Vec<u16> = value.encode_wide().chain(std::iter::once(0)).collect();
    let len = wide.len() * std::mem::size_of::<u16>();
    let ptr = wide.as_ptr() as *const u8;
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

struct OwnedRegKey(HKEY);

impl Drop for OwnedRegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Launch the GUI window. Blocks the calling thread.
pub fn run_gui(control: GuiControl) -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon_256.png"))
        .expect("embedded icon PNG is valid");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EternalMonitor // SIGNAL")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([760.0, 480.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    eframe::run_native(
        "EternalMonitor",
        options,
        Box::new(|cc| Ok(Box::new(AnalyzerApp::new(cc, control)))),
    )
}
