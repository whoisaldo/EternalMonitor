import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settings: AppSettings
    @Environment(\.dismiss) private var dismiss

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "—"
        let build = info?["CFBundleVersion"] as? String ?? "—"
        return "\(version) (\(build))"
    }

    var body: some View {
        NavigationStack {
            Form {
                // Playback (local device preferences)
                Section {
                    Picker(selection: $settings.targetFPS) {
                        Text("30 fps").tag(30)
                        Text("60 fps").tag(60)
                    } label: {
                        Label("Frame Rate", systemImage: "speedometer")
                    }

                    Toggle(isOn: $settings.promotionEnabled) {
                        Label("ProMotion (120Hz)", systemImage: "display")
                    }

                    Toggle(isOn: $settings.showHUD) {
                        Label("Show stats HUD", systemImage: "gauge.with.dots.needle.33percent")
                    }

                    Toggle(isOn: $settings.autoReconnect) {
                        Label("Auto-reconnect", systemImage: "arrow.triangle.2.circlepath")
                    }
                } header: {
                    sectionHeader("Playback")
                } footer: {
                    footnote("Triple-tap the video to toggle the HUD while streaming.")
                }

                // Connection
                Section {
                    Toggle(isOn: $settings.preferUSB) {
                        Label("Prefer USB when available", systemImage: "cable.connector")
                    }
                } header: {
                    sectionHeader("Connection")
                }

                // Host stream — honest about what the iPad controls vs the PC
                Section {
                    infoRow("Resolution", "Matches your PC display", "rectangle.on.rectangle")
                    infoRow("Codec", "H.264", "cpu")
                    infoRow("Bitrate", "Set on the host", "slider.horizontal.3")
                } header: {
                    sectionHeader("Host stream")
                } footer: {
                    footnote("Resolution, codec and bitrate are configured in the EternalMonitor app on your Windows PC.")
                }

                // About
                Section {
                    HStack {
                        Text("Version")
                            .foregroundColor(Theme.text)
                        Spacer()
                        Text(appVersion)
                            .font(.appMonoRegular(size: 13))
                            .foregroundColor(Theme.text2)
                    }
                    HStack {
                        Text("Build")
                            .foregroundColor(Theme.text)
                        Spacer()
                        Text("SIGNAL")
                            .font(.appMonoMedium(size: 12))
                            .tracking(2)
                            .foregroundColor(Theme.amber)
                    }
                } header: {
                    sectionHeader("About")
                }
            }
            .scrollContentBackground(.hidden)
            .background(Theme.void.ignoresSafeArea())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .font(.appMonoMedium(size: 15))
                        .foregroundColor(Theme.amber)
                }
            }
            .tint(Theme.amber)
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(title.uppercased())
            .font(.appMonoMedium(size: 11))
            .tracking(1.5)
            .foregroundColor(Theme.text3)
    }

    private func footnote(_ text: String) -> some View {
        Text(text)
            .font(.appMonoRegular(size: 11))
            .foregroundColor(Theme.text3)
    }

    private func infoRow(_ title: String, _ value: String, _ symbol: String) -> some View {
        HStack {
            Label(title, systemImage: symbol)
                .foregroundColor(Theme.text)
            Spacer()
            Text(value)
                .font(.appMonoRegular(size: 13))
                .foregroundColor(Theme.text2)
        }
    }
}
