use std::ffi::OsStr;
use std::net::SocketAddr;
use std::os::windows::ffi::OsStrExt;

use eframe::egui;
use tracing::{info, warn};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPEN_CREATE_OPTIONS, REG_OPTION_NON_VOLATILE,
    REG_SZ,
};

use crate::control::GuiControl;
use crate::logging::{session_log_path, session_log_text};
use crate::stats::PIPELINE_STATS;

const BG: egui::Color32 = egui::Color32::from_rgb(10, 10, 10);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(17, 17, 17);
const SURFACE2: egui::Color32 = egui::Color32::from_rgb(28, 28, 28);
const BORDER: egui::Color32 = egui::Color32::from_rgba_premultiplied(255, 255, 255, 15);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(232, 255, 71);
const GREEN: egui::Color32 = egui::Color32::from_rgb(29, 158, 117);
const RED: egui::Color32 = egui::Color32::from_rgb(226, 75, 74);
const MUTED: egui::Color32 = egui::Color32::from_rgb(85, 85, 85);
const MUTED2: egui::Color32 = egui::Color32::from_rgb(136, 136, 136);
const TEXT: egui::Color32 = egui::Color32::from_rgb(240, 240, 240);
const CLEAR: egui::Color32 = egui::Color32::TRANSPARENT;
const SELECTION_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(45, 50, 14, 50);
const DANGER_BORDER: egui::Color32 = egui::Color32::from_rgba_premultiplied(45, 15, 15, 51);
const DANGER_HOVER_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(18, 6, 6, 20);
const PILL_GREEN_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(3, 15, 11, 25);
const PILL_GREEN_BORDER: egui::Color32 = egui::Color32::from_rgba_premultiplied(6, 32, 23, 51);
const PILL_RED_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(18, 6, 6, 20);
const PILL_RED_BORDER: egui::Color32 = egui::Color32::from_rgba_premultiplied(45, 15, 15, 51);
const SPARKLINE_FILL_ALPHA: u8 = 13;

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
    capture_resolution: (u32, u32),
    encode_fps: f64,
    encode_time_us: u128,
    encode_frame_count: u64,
    nal_bytes_last: usize,
    bitrate_bps: u32,
    codec_name: String,
    gpu_name: String,
    transport_fps: f64,
    transport_bytes_sent: u64,
    transport_packets_sent: u64,
    transport_fragments_sent: u64,
    target_addr: String,
    latency_ms: f64,
    bandwidth_bps: f64,
    bandwidth_mbps: f64,
    encode_time_history: Vec<f64>,
    pipeline_running: bool,
    uptime_secs: f64,
    mdns_active: bool,
}

impl StatsSnapshot {
    fn take() -> Self {
        let s = PIPELINE_STATS.lock();
        Self {
            listen_addr: s.listen_addr.clone(),
            capture_fps: s.capture_fps,
            capture_resolution: s.capture_resolution,
            encode_fps: s.encode_fps,
            encode_time_us: s.encode_time_us,
            encode_frame_count: s.encode_frame_count,
            nal_bytes_last: s.nal_bytes_last,
            bitrate_bps: s.bitrate_bps,
            codec_name: s.codec_name.clone(),
            gpu_name: s.gpu_name.clone(),
            transport_fps: s.transport_fps,
            transport_bytes_sent: s.transport_bytes_sent,
            transport_packets_sent: s.transport_packets_sent,
            transport_fragments_sent: s.transport_fragments_sent,
            target_addr: s.target_addr.clone(),
            latency_ms: s.latency_ms,
            bandwidth_bps: s.bandwidth_bps,
            bandwidth_mbps: s.bandwidth_mbps,
            encode_time_history: s.encode_time_history.iter().copied().collect(),
            pipeline_running: s.pipeline_running,
            uptime_secs: s.uptime_secs(),
            mdns_active: s.mdns_active,
        }
    }
}

pub struct AnalyzerApp {
    control: GuiControl,
    current_tab: AppTab,
    settings_bitrate_mbps: f32,
    settings_fps_target: u32,
    settings_target_ip: String,
    settings_target_error: Option<String>,
    settings_start_on_boot: bool,
    show_qr_modal: bool,
}

impl AnalyzerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, control: GuiControl) -> Self {
        let target_addr = *control.shared.target_addr.lock();
        let settings_target_ip = if target_addr.ip().is_unspecified() || target_addr.port() == 0 {
            String::new()
        } else {
            target_addr.to_string()
        };

        Self {
            settings_bitrate_mbps: control.shared.bitrate_bps.load(std::sync::atomic::Ordering::SeqCst)
                as f32
                / 1_000_000.0,
            settings_start_on_boot: read_startup_registry(),
            settings_target_ip,
            control,
            current_tab: AppTab::Stream,
            settings_fps_target: 60,
            settings_target_error: None,
            show_qr_modal: false,
        }
    }

    fn apply_target_addr(&mut self) {
        match self.settings_target_ip.trim().parse::<SocketAddr>() {
            Ok(target_addr) => {
                *self.control.shared.target_addr.lock() = target_addr;
                PIPELINE_STATS.lock().set_target_addr(target_addr.to_string());
                self.settings_target_error = None;
                info!(target = %target_addr, "Transport target updated from GUI");
            }
            Err(error) => {
                self.settings_target_error = Some("Enter host:port".to_string());
                warn!(error = %error, target = %self.settings_target_ip, "Invalid target address");
            }
        }
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
            egui::Window::new("QR Code")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(
                        egui::RichText::new("Connect this target from the iPad app.")
                            .color(TEXT)
                            .size(14.0),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}",
                            value_or_unknown(&snap.listen_addr)
                        ))
                        .color(MUTED)
                        .monospace()
                        .size(13.0),
                    );
                    ui.add_space(8.0);
                    if ui.add(egui::Button::new("Close").corner_radius(8.0)).clicked() {
                        self.show_qr_modal = false;
                    }
                });
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
            (SURFACE2, TEXT)
        } else {
            (CLEAR, MUTED)
        };

        let btn = egui::Button::new(
            egui::RichText::new(label).color(text_color).size(14.0),
        )
        .fill(bg)
        .stroke(egui::Stroke::NONE)
        .corner_radius(4.0);

        if ui.add_sized([ui.available_width(), 28.0], btn).clicked() {
            self.current_tab = tab;
        }
        ui.add_space(4.0);
    }

    fn draw_stream_tab(&mut self, ui: &mut egui::Ui, snap: &StatsSnapshot) {
        ui.add_space(8.0);
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(value_or_unknown(&snap.listen_addr))
                        .color(TEXT)
                        .monospace()
                        .size(16.0),
                );
                ui.add_space(12.0);
                if ui.add(egui::Button::new("Copy IP").corner_radius(8.0)).clicked() {
                    if !snap.listen_addr.is_empty() {
                        ui.ctx().copy_text(snap.listen_addr.clone());
                    }
                }
                if ui.add(egui::Button::new("QR Code").corner_radius(8.0)).clicked() {
                    self.show_qr_modal = true;
                }
            });
        });

        ui.add_space(12.0);

        ui.horizontal(|ui| {
            metric_card(ui, "FPS", &format!("{:.1}", snap.capture_fps), ACCENT);
            metric_card(
                ui,
                "Encode ms",
                &format!("{:.2}", snap.encode_time_us as f64 / 1000.0),
                ACCENT,
            );
            metric_card(ui, "Latency ms", &format!("{:.1}", snap.latency_ms), GREEN);
            metric_card(ui, "Mbps", &format!("{:.1}", snap.bandwidth_mbps), ACCENT);
        });

        ui.add_space(12.0);

        ui.columns(2, |cols| {
            card_frame().show(&mut cols[0], |ui| {
                section_header(ui, "Encoder");
                stat_row(ui, "GPU", value_or_unknown(&snap.gpu_name));
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
                section_header(ui, "Encode Time (ms)");
                draw_sparkline(ui, &snap.encode_time_history, 120.0, ACCENT);
            });
        });

        ui.add_space(12.0);

        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("Restart stream").corner_radius(8.0)).clicked() {
                self.control.request_restart();
            }
            let copy_logs_button = ui.add_enabled(
                session_log_text().is_some(),
                egui::Button::new("Copy logs").corner_radius(8.0),
            );
            if copy_logs_button.clicked() {
                if let Some(log_text) = session_log_text() {
                    ui.ctx().copy_text(log_text);
                }
            }
            copy_logs_button.clone().on_hover_text(format!(
                "Copies the full session log from {}",
                session_log_path().display()
            ));
            if copy_logs_button.hovered() && !copy_logs_button.enabled() {
                copy_logs_button.on_hover_text("No recent log lines have been captured yet.");
            }
            ui.scope(|ui| {
                let v = ui.visuals_mut();
                v.widgets.inactive.weak_bg_fill = CLEAR;
                v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, DANGER_BORDER);
                v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, RED);
                v.widgets.hovered.weak_bg_fill = DANGER_HOVER_FILL;
                v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, DANGER_BORDER);
                v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, RED);
                if ui.add(egui::Button::new("Stop").corner_radius(8.0)).clicked() {
                    self.control.shared.stop();
                }
            });
        });
    }

    fn draw_performance_tab(&self, ui: &mut egui::Ui, snap: &StatsSnapshot) {
        ui.add_space(8.0);
        section_header(ui, "Encode Time (ms)");
        draw_sparkline(ui, &snap.encode_time_history, 200.0, ACCENT);

        ui.add_space(16.0);

        ui.horizontal(|ui| {
            metric_card(ui, "Capture FPS", &format!("{:.1}", snap.capture_fps), ACCENT);
            metric_card(ui, "Encode FPS", &format!("{:.1}", snap.encode_fps), ACCENT);
            metric_card(ui, "Transport FPS", &format!("{:.1}", snap.transport_fps), ACCENT);
        });

        ui.add_space(16.0);

        card_frame().show(ui, |ui| {
            stat_row(ui, "Packets sent", &snap.transport_packets_sent.to_string());
            stat_row(ui, "Fragments sent", &snap.transport_fragments_sent.to_string());
            stat_row(ui, "Bytes sent", &format_bytes(snap.transport_bytes_sent));
            stat_row(ui, "Bandwidth", &format!("{:.1} Mbps", snap.bandwidth_mbps));
            stat_row(ui, "Bits/sec", &format!("{:.0}", snap.bandwidth_bps));
            stat_row(ui, "Uptime", &format_uptime(snap.uptime_secs));
            stat_row(ui, "Target", value_or_unknown(&snap.target_addr));
            stat_row(ui, "mDNS", if snap.mdns_active { "Active" } else { "Inactive" });
        });
    }

    fn draw_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        section_header(ui, "Settings");

        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Bitrate").color(TEXT).size(13.0));
                let slider = egui::Slider::new(&mut self.settings_bitrate_mbps, 1.0..=50.0).text("Mbps");
                if ui.add(slider).changed() {
                    let bitrate_bps = (self.settings_bitrate_mbps * 1_000_000.0).round() as u32;
                    self.control
                        .shared
                        .bitrate_bps
                        .store(bitrate_bps, std::sync::atomic::Ordering::SeqCst);
                    PIPELINE_STATS.lock().set_bitrate(bitrate_bps);
                }
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("FPS target").color(TEXT).size(13.0));
                ui.add_space(8.0);
                let response = ui.add_enabled_ui(false, |ui| {
                    let _ = ui.selectable_label(self.settings_fps_target == 30, "30");
                    let _ = ui.selectable_label(self.settings_fps_target == 60, "60");
                });
                response
                    .response
                    .on_hover_text("Runtime FPS switching is not wired in this build.");
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Target IP").color(TEXT).size(13.0));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.settings_target_ip).desired_width(220.0),
                );
                let enter_pressed =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.scope(|ui| {
                    let v = ui.visuals_mut();
                    v.widgets.inactive.weak_bg_fill = ACCENT;
                    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, BG);
                    v.widgets.hovered.weak_bg_fill = ACCENT;
                    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, BG);
                    if ui.add(egui::Button::new("Apply").corner_radius(8.0)).clicked() || enter_pressed {
                        self.apply_target_addr();
                    }
                });
            });
            if let Some(error) = &self.settings_target_error {
                ui.label(egui::RichText::new(error).color(RED).size(11.0));
            }

            ui.add_space(8.0);

            let prev = self.settings_start_on_boot;
            ui.checkbox(
                &mut self.settings_start_on_boot,
                egui::RichText::new("Start on Windows startup").color(TEXT).size(13.0),
            );
            if self.settings_start_on_boot != prev {
                if let Err(error) = set_startup_registry(self.settings_start_on_boot) {
                    self.settings_start_on_boot = prev;
                    self.settings_target_error = Some(error);
                } else {
                    self.settings_target_error = None;
                }
            }
        });
    }
}

fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(8))
}


fn logo_widget(ui: &mut egui::Ui) {
    let start = ui.cursor().min;
    let bar_w = 28.0;
    let bar_h = 4.0;
    let gap = 3.0;
    for i in 0..3 {
        let y = start.y + i as f32 * (bar_h + gap);
        let rect = egui::Rect::from_min_size(egui::pos2(start.x, y), egui::vec2(bar_w, bar_h));
        ui.painter().rect_filled(rect, 1.0, ACCENT);
    }
    ui.add_space(3.0 * (bar_h + gap) + 4.0);
    ui.label(
        egui::RichText::new("EternalMonitor")
            .color(ACCENT)
            .size(16.0)
            .strong(),
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
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let dot_color = if running {
                    ui.ctx().request_repaint();
                    let t = (ctx.input(|i| i.time) % 2.0) as f32;
                    let alpha = if t < 1.0 { 0.4 + 0.6 * t } else { 1.6 - 0.6 * t };
                    let a = (255.0 * alpha) as u8;
                    egui::Color32::from_rgba_unmultiplied(GREEN.r(), GREEN.g(), GREEN.b(), a)
                } else {
                    RED
                };
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, dot_color);

                let label = if running { "STREAMING" } else { "STOPPED" };
                let label_color = if running { GREEN } else { RED };
                ui.label(
                    egui::RichText::new(label)
                        .color(label_color)
                        .size(11.0)
                        .strong(),
                );
            });
            if !target_addr.is_empty() {
                ui.label(egui::RichText::new(target_addr).color(MUTED).size(10.0));
            }
        });
}

fn metric_card(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    card_frame().show(ui, |ui| {
        ui.set_min_width(100.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(label).color(MUTED).size(11.0));
            ui.label(egui::RichText::new(value).color(color).size(22.0).strong().monospace());
        });
    });
}

fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text.to_uppercase()).color(MUTED).size(10.0));
    ui.add_space(4.0);
}

fn stat_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(MUTED).size(11.0).monospace());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).color(TEXT).size(11.0).monospace());
        });
    });
}

fn draw_sparkline(ui: &mut egui::Ui, data: &[f64], height: f32, color: egui::Color32) {
    if data.len() < 2 {
        ui.label(egui::RichText::new("Waiting for data...").color(MUTED));
        return;
    }

    let min_val = data.iter().cloned().fold(f64::MAX, f64::min);
    let max_val = data.iter().cloned().fold(f64::MIN, f64::max);
    let range = (max_val - min_val).max(0.001);

    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter();

    painter.rect_filled(rect, 4.0, SURFACE2);

    let points: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = rect.left() + (i as f32 / (data.len() - 1) as f32) * rect.width();
            let y = rect.bottom() - 4.0 - (((v - min_val) / range) as f32 * (height - 8.0));
            egui::pos2(x, y)
        })
        .collect();

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

    painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
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
            .with_title("EternalMonitor")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([700.0, 450.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    eframe::run_native(
        "EternalMonitor",
        options,
        Box::new(|cc| Ok(Box::new(AnalyzerApp::new(cc, control)))),
    )
}
