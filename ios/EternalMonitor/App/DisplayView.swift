import SwiftUI

struct DisplayView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings

    @State private var showHUD = true
    @State private var hudDismissTask: Task<Void, Never>?
    // connection quality popover state.
    @State private var showQualityPopover = false

    var body: some View {
        ZStack {
            // Full-bleed video
            MetalView()
                .ignoresSafeArea()
                .onTapGesture(count: 3) {
                    // Triple-tap to toggle HUD
                    showHUD.toggle()
                    scheduleHUDDismiss()
                }
                .onTapGesture(count: 1) {
                    if showHUD {
                        showHUD = false
                    }
                }

            // HUD + bottom bar overlay
            VStack {
                // Top-right stats HUD
                HStack {
                    Spacer()
                    if showHUD && settings.showHUD {
                        hudOverlay
                            .transition(.opacity.combined(with: .move(edge: .top)))
                    }
                }
                .padding(.top, 8)
                .padding(.trailing, 16)

                Spacer()

                // Bottom status bar
                bottomBar
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
            }
        }
        .statusBarHidden(true)
        .persistentSystemOverlays(.hidden)
        .onAppear {
            scheduleHUDDismiss()
        }
        .onDisappear {
            hudDismissTask?.cancel()
        }
    }

    // MARK: - HUD overlay

    private var hudOverlay: some View {
        HStack(spacing: 12) {
            statLabel("\(Int(connectionManager.fps))", unit: "fps")
            divider
            statLabel(String(format: "%.1f", connectionManager.lagMs), unit: "ms")
            divider
            statLabel(connectionManager.transportMode, unit: "")
            divider
            qualityBars
                .onTapGesture { showQualityPopover.toggle() }
                .popover(isPresented: $showQualityPopover) {
                    qualityPopover
                        .padding(12)
                        .presentationCompactAdaptation(.popover)
                }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(.ultraThinMaterial)
                .environment(\.colorScheme, .dark)
        )
    }

    // 4-bar signal strength indicator.
    private var qualityBars: some View {
        let bars = connectionManager.quality.bars
        let color = qualityColor(bars: bars)
        return HStack(alignment: .bottom, spacing: 2) {
            ForEach(0..<4, id: \.self) { idx in
                Rectangle()
                    .fill(idx < bars ? color : Color.white.opacity(0.15))
                    .frame(width: 3, height: CGFloat(4 + idx * 3))
                    .cornerRadius(0.5)
            }
        }
        .frame(height: 13)
        .contentShape(Rectangle())
    }

    private func qualityColor(bars: Int) -> Color {
        switch bars {
        case 4: return Color(hex: 0x1D9E75)
        case 3: return Color(hex: 0xB7E84A)
        case 2: return Color(hex: 0xE8C547)
        default: return Color(hex: 0xE24B4A)
        }
    }

    // popover with exact quality numbers.
    private var qualityPopover: some View {
        let q = connectionManager.quality
        return VStack(alignment: .leading, spacing: 6) {
            Text("Connection quality")
                .font(.appMonoMedium(size: 11))
                .foregroundColor(.white.opacity(0.5))
            HStack(spacing: 12) {
                statLabel(String(format: "%.1f", q.lossPercent), unit: "% loss")
                statLabel(String(format: "%.1f", q.jitterMs), unit: "ms jitter")
                statLabel("\(q.seqGap)", unit: "max seq gap")
            }
        }
    }

    private func statLabel(_ value: String, unit: String) -> some View {
        HStack(spacing: 3) {
            Text(value)
                .font(.appMonoMedium(size: 12))
                .foregroundColor(.white)
            if !unit.isEmpty {
                Text(unit)
                    .font(.appMonoRegular(size: 10))
                    .foregroundColor(.white.opacity(0.5))
            }
        }
    }

    private var divider: some View {
        Rectangle()
            .fill(Color.white.opacity(0.2))
            .frame(width: 1, height: 12)
    }

    // MARK: - Bottom bar

    private var bottomBar: some View {
        HStack {
            // Connection pill
            HStack(spacing: 6) {
                Circle()
                    .fill(Color(hex: 0x1D9E75))
                    .frame(width: 8, height: 8)

                Text("Connected")
                    .font(.appMonoRegular(size: 12))
                    .foregroundColor(.white.opacity(0.7))

                Text("·")
                    .foregroundColor(.white.opacity(0.3))

                Text(connectionManager.transportMode)
                    .font(.appMonoMedium(size: 12))
                    .foregroundColor(Color(hex: 0x1D9E75))
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .background(
                Capsule()
                    .fill(Color.white.opacity(0.06))
                    .overlay(
                        Capsule()
                            .strokeBorder(Color.white.opacity(0.08), lineWidth: 0.5)
                    )
            )

            Spacer()

            // Disconnect button
            Button {
                connectionManager.disconnect()
            } label: {
                Text("Disconnect")
                    .font(.appMonoMedium(size: 12))
                    .foregroundColor(Color(hex: 0xe8ff47))
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .background(
                        Capsule()
                            .fill(Color(hex: 0xe8ff47).opacity(0.1))
                    )
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 20)
                .fill(.ultraThinMaterial)
                .environment(\.colorScheme, .dark)
        )
    }

    // MARK: - Auto-hide HUD

    private func scheduleHUDDismiss() {
        hudDismissTask?.cancel()
        showHUD = true
        hudDismissTask = Task {
            try? await Task.sleep(for: .seconds(5))
            if !Task.isCancelled {
                withAnimation(.easeOut(duration: 0.3)) {
                    showHUD = false
                }
            }
        }
    }
}
