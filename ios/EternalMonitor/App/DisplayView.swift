import SwiftUI

struct DisplayView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings

    @State private var showHUD = true
    @State private var hudDismissTask: Task<Void, Never>?
    @State private var showQualityPopover = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            // Full-bleed video
            MetalView()
                .ignoresSafeArea()
                .onTapGesture(count: 3) {
                    showHUD.toggle()
                    scheduleHUDDismiss()
                }
                .onTapGesture(count: 1) {
                    if showHUD { withAnimation(.easeOut(duration: 0.25)) { showHUD = false } }
                }

            // Viewfinder registration marks frame the picture (fade with the HUD).
            if showHUD {
                ViewfinderCorners(armLength: 26, inset: 18)
                    .stroke(Theme.amber.opacity(0.55), lineWidth: 1.5)
                    .ignoresSafeArea()
                    .transition(.opacity)
                    .allowsHitTesting(false)
            }

            VStack {
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

                if showHUD {
                    bottomBar
                        .padding(.horizontal, 16)
                        .padding(.bottom, 8)
                        .transition(.opacity.combined(with: .move(edge: .bottom)))
                }
            }
        }
        .statusBarHidden(true)
        .persistentSystemOverlays(.hidden)
        .onAppear { scheduleHUDDismiss() }
        .onDisappear { hudDismissTask?.cancel() }
    }

    // MARK: - HUD overlay

    private var hudOverlay: some View {
        HStack(spacing: 14) {
            stat("\(Int(connectionManager.fps))", unit: "fps", color: Theme.amber)
            divider
            stat(String(format: "%.0f", connectionManager.lagMs), unit: "ms", color: Theme.phosphor)
            divider
            stat(connectionManager.transportMode, unit: "", color: Theme.text)
            divider
            qualityBars
                .onTapGesture { showQualityPopover.toggle() }
                .popover(isPresented: $showQualityPopover) {
                    qualityPopover
                        .padding(14)
                        .background(Theme.panel)
                        .presentationCompactAdaptation(.popover)
                }
                .accessibilityLabel("Connection quality details")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .background(
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .fill(.ultraThinMaterial)
                .environment(\.colorScheme, .dark)
                .overlay(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(Theme.hairline, lineWidth: 1)
                )
        )
    }

    private var qualityBars: some View {
        let bars = connectionManager.quality.bars
        let color = Theme.quality(bars: bars)
        return HStack(alignment: .bottom, spacing: 2) {
            ForEach(0..<4, id: \.self) { idx in
                RoundedRectangle(cornerRadius: 0.5)
                    .fill(idx < bars ? color : Color.white.opacity(0.15))
                    .frame(width: 3, height: CGFloat(4 + idx * 3))
            }
        }
        .frame(height: 13)
        .contentShape(Rectangle())
    }

    private var qualityPopover: some View {
        let q = connectionManager.quality
        return VStack(alignment: .leading, spacing: 10) {
            SectionLabel(title: "Connection quality")
            HStack(spacing: 18) {
                Readout(value: String(format: "%.1f", q.lossPercent), unit: "%", label: "loss", color: q.lossPercent < 3 ? Theme.phosphor : Theme.caution)
                Readout(value: String(format: "%.0f", q.jitterMs), unit: "ms", label: "jitter", color: Theme.text)
                Readout(value: "\(q.seqGap)", unit: "", label: "max gap", color: Theme.text)
            }
        }
    }

    private func stat(_ value: String, unit: String, color: Color) -> some View {
        HStack(spacing: 3) {
            Text(value)
                .font(.appMonoMedium(size: 13))
                .foregroundColor(color)
            if !unit.isEmpty {
                Text(unit)
                    .font(.appMonoRegular(size: 10))
                    .foregroundColor(Theme.text2)
            }
        }
    }

    private var divider: some View {
        Rectangle()
            .fill(Color.white.opacity(0.18))
            .frame(width: 1, height: 13)
    }

    // MARK: - Bottom bar

    private var bottomBar: some View {
        HStack {
            HStack(spacing: 8) {
                SignalDot(color: Theme.phosphor, size: 8)
                Text("ON AIR")
                    .font(.appMonoMedium(size: 12))
                    .tracking(1)
                    .foregroundColor(Theme.phosphor)
                Text("·")
                    .foregroundColor(Theme.text3)
                Text(connectionManager.transportMode)
                    .font(.appMonoRegular(size: 12))
                    .foregroundColor(Theme.text2)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .background(
                Capsule()
                    .fill(Color.white.opacity(0.06))
                    .overlay(Capsule().strokeBorder(Theme.hairline, lineWidth: 0.5))
            )

            Spacer()

            Button {
                connectionManager.disconnect()
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "stop.fill").font(.system(size: 10))
                    Text("Disconnect")
                        .font(.appMonoMedium(size: 12))
                }
                .foregroundColor(Theme.amber)
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(Capsule().fill(Theme.amber.opacity(0.12)))
            }
            .accessibilityLabel("Disconnect")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 22, style: .continuous)
                .fill(.ultraThinMaterial)
                .environment(\.colorScheme, .dark)
        )
    }

    // MARK: - Auto-hide HUD

    private func scheduleHUDDismiss() {
        hudDismissTask?.cancel()
        withAnimation(.easeOut(duration: 0.25)) { showHUD = true }
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
