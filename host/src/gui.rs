use eframe::egui;

use crate::stats::PIPELINE_STATS;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(0xe8, 0xff, 0x47);
const TEAL: egui::Color32 = egui::Color32::from_rgb(0x1d, 0x9e, 0x75);
const BG: egui::Color32 = egui::Color32::from_rgb(0x0a, 0x0a, 0x0a);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(0x16, 0x16, 0x16);
const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(0x88, 0x88, 0x88);

pub struct AnalyzerApp;

impl AnalyzerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self
    }
}

impl eframe::App for AnalyzerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Repaint continuously for real-time updates
        ctx.request_repaint_after(std::time::Duration::from_millis(50));

        // Dark theme
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = SURFACE;
        visuals.widgets.noninteractive.bg_fill = SURFACE;
        ctx.set_visuals(visuals);

        let stats = PIPELINE_STATS.lock();

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(
                    egui::RichText::new("EternalMonitor")
                        .color(ACCENT)
                        .size(20.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new("Real-Time Analyzer")
                        .color(TEXT_DIM)
                        .size(14.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status_text = if stats.pipeline_running {
                        "LIVE"
                    } else {
                        "STOPPED"
                    };
                    let status_color = if stats.pipeline_running {
                        TEAL
                    } else {
                        egui::Color32::RED
                    };
                    ui.label(
                        egui::RichText::new(format!("● {}", status_text))
                            .color(status_color)
                            .size(14.0),
                    );
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);

            // --- Top row: key metrics ---
            ui.horizontal(|ui| {
                metric_card(
                    ui,
                    "Capture FPS",
                    &format!("{:.1}", stats.capture_fps),
                    ACCENT,
                );
                metric_card(
                    ui,
                    "Encode FPS",
                    &format!("{:.1}", stats.encode_fps),
                    ACCENT,
                );
                metric_card(
                    ui,
                    "Transport FPS",
                    &format!("{:.1}", stats.transport_fps),
                    ACCENT,
                );

                let bw = if stats.bandwidth_bps > 1_000_000.0 {
                    format!("{:.1} Mbps", stats.bandwidth_bps / 1_000_000.0)
                } else if stats.bandwidth_bps > 1_000.0 {
                    format!("{:.0} Kbps", stats.bandwidth_bps / 1_000.0)
                } else {
                    format!("{:.0} bps", stats.bandwidth_bps)
                };
                metric_card(ui, "Bandwidth", &bw, TEAL);
            });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            // --- Two-column layout ---
            ui.columns(2, |cols| {
                // Left column: pipeline details
                cols[0].group(|ui| {
                    section_header(ui, "Capture");
                    stat_row(
                        ui,
                        "Resolution",
                        &format!(
                            "{}x{}",
                            stats.capture_resolution.0, stats.capture_resolution.1
                        ),
                    );
                    stat_row(ui, "Frames", &stats.capture_frame_count.to_string());
                });

                cols[0].add_space(8.0);

                cols[0].group(|ui| {
                    section_header(ui, "Encoder");
                    stat_row(ui, "Codec", "H.264 (NVENC)");
                    stat_row(
                        ui,
                        "Encode Time",
                        &format!("{:.2} ms", stats.encode_time_us as f64 / 1000.0),
                    );
                    stat_row(ui, "NAL Size", &format_bytes(stats.nal_bytes_last as u64));
                    stat_row(ui, "Frames", &stats.encode_frame_count.to_string());
                });

                cols[0].add_space(8.0);

                cols[0].group(|ui| {
                    section_header(ui, "Transport");
                    stat_row(ui, "Target", &stats.target_addr);
                    stat_row(ui, "Total Sent", &format_bytes(stats.transport_bytes_sent));
                    stat_row(ui, "Packets", &stats.transport_packets_sent.to_string());
                    stat_row(ui, "Fragments", &stats.transport_fragments_sent.to_string());
                });

                // Right column: encode time chart + status
                cols[1].group(|ui| {
                    section_header(ui, "Encode Time (ms)");
                    let history: Vec<f64> = stats.encode_time_history.iter().copied().collect();
                    if history.len() >= 2 {
                        let max_val = history.iter().cloned().fold(1.0_f64, f64::max);
                        let chart_height = 120.0;
                        let (response, painter) = ui.allocate_painter(
                            egui::vec2(ui.available_width(), chart_height),
                            egui::Sense::hover(),
                        );
                        let rect = response.rect;

                        // Background
                        painter.rect_filled(rect, 4.0, SURFACE);

                        // Grid lines
                        for i in 1..4 {
                            let y = rect.top() + (i as f32 / 4.0) * chart_height;
                            painter.line_segment(
                                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                                egui::Stroke::new(0.5, egui::Color32::from_rgb(0x30, 0x30, 0x30)),
                            );
                        }

                        // Line chart
                        let points: Vec<egui::Pos2> = history
                            .iter()
                            .enumerate()
                            .map(|(i, &v)| {
                                let x = rect.left()
                                    + (i as f32 / (history.len() - 1) as f32) * rect.width();
                                let y = rect.bottom()
                                    - ((v / max_val) as f32 * (chart_height - 8.0))
                                    - 4.0;
                                egui::pos2(x, y)
                            })
                            .collect();

                        if points.len() >= 2 {
                            painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, ACCENT)));
                        }

                        // Scale label
                        ui.label(
                            egui::RichText::new(format!("max: {:.1} ms", max_val))
                                .color(TEXT_DIM)
                                .size(10.0),
                        );
                    } else {
                        ui.label(egui::RichText::new("Waiting for data...").color(TEXT_DIM));
                    }
                });

                cols[1].add_space(8.0);

                cols[1].group(|ui| {
                    section_header(ui, "Status");

                    let uptime = stats.uptime_secs();
                    let mins = (uptime / 60.0).floor() as u64;
                    let secs = (uptime % 60.0).floor() as u64;
                    stat_row(ui, "Uptime", &format!("{}m {}s", mins, secs));
                    stat_row(
                        ui,
                        "mDNS Discovery",
                        if stats.mdns_active {
                            "Active"
                        } else {
                            "Inactive"
                        },
                    );
                });
            });
        });
    }
}

fn metric_card(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    ui.group(|ui| {
        ui.set_min_width(120.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(label).color(TEXT_DIM).size(11.0));
            ui.label(egui::RichText::new(value).color(color).size(22.0).strong());
        });
    });
}

fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(ACCENT).size(14.0).strong());
    ui.add_space(4.0);
}

fn stat_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(TEXT_DIM).size(12.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .color(egui::Color32::WHITE)
                    .size(12.0),
            );
        });
    });
}

fn format_bytes(bytes: u64) -> String {
    if bytes > 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes > 1_000_000 {
        format!("{:.2} MB", bytes as f64 / 1_000_000.0)
    } else if bytes > 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Launch the analyzer GUI window. Blocks the calling thread.
pub fn run_gui() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EternalMonitor — Analyzer")
            .with_inner_size([720.0, 520.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "EternalMonitor Analyzer",
        options,
        Box::new(|cc| Ok(Box::new(AnalyzerApp::new(cc)))),
    )
}
