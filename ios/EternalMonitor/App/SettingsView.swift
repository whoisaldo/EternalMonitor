import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settings: AppSettings
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                // Display
                Section {
                    HStack {
                        Label("Resolution", systemImage: "rectangle.on.rectangle")
                        Spacer()
                        Text("1920 × 1080")
                            .font(.appMonoRegular(size: 13))
                            .foregroundColor(.secondary)
                    }

                    Picker(selection: $settings.targetFPS) {
                        Text("30 fps").tag(30)
                        Text("60 fps").tag(60)
                    } label: {
                        Label("Frame Rate", systemImage: "speedometer")
                    }

                    Toggle(isOn: $settings.promotionEnabled) {
                        Label("ProMotion (120Hz)", systemImage: "display")
                    }
                } header: {
                    sectionHeader("Display")
                }

                // Encoding
                Section {
                    HStack {
                        Label("Codec", systemImage: "cpu")
                        Spacer()
                        Text("H.264 Baseline")
                            .font(.appMonoRegular(size: 13))
                            .foregroundColor(.secondary)
                    }

                    HStack {
                        Label("Quality", systemImage: "slider.horizontal.3")
                        Spacer()
                        Text("15 Mbps CBR")
                            .font(.appMonoRegular(size: 13))
                            .foregroundColor(.secondary)
                    }
                } header: {
                    sectionHeader("Encoding")
                }

                // Connection
                Section {
                    Toggle(isOn: $settings.preferUSB) {
                        Label("Prefer USB", systemImage: "cable.connector")
                    }

                    Toggle(isOn: $settings.showHUD) {
                        Label("Show HUD", systemImage: "gauge.with.dots.needle.33percent")
                    }

                    Toggle(isOn: $settings.autoReconnect) {
                        Label("Auto-Reconnect", systemImage: "arrow.triangle.2.circlepath")
                    }
                } header: {
                    sectionHeader("Connection")
                }

                // About
                Section {
                    HStack {
                        Text("Version")
                        Spacer()
                        Text("0.1.0")
                            .font(.appMonoRegular(size: 13))
                            .foregroundColor(.secondary)
                    }
                } header: {
                    sectionHeader("About")
                }
            }
            .scrollContentBackground(.hidden)
            .background(Color(hex: 0x080808))
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .foregroundColor(Color(hex: 0xe8ff47))
                }
            }
            .tint(Color(hex: 0xe8ff47))
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(title)
            .font(.appMonoMedium(size: 11))
            .foregroundColor(.white.opacity(0.4))
    }
}
