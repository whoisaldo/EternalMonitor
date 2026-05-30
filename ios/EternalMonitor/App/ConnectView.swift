import SwiftUI

struct ConnectView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings
    @StateObject private var recentStore = RecentConnectionStore.shared
    @StateObject private var scanner = NetworkScanner()

    @State private var hostIP: String = ""
    @State private var port: String = "9876"
    @State private var showSettings = false
    // track whether we've already pre-filled from lastHost so we
    // don't clobber user edits on every re-render.
    @State private var didPrefillFromLastHost = false
    @State private var showQRScanner = false
    @State private var appeared = false
    @AppStorage("didSeeOnboarding") private var didSeeOnboarding = false
    @FocusState private var focusedField: Field?

    private enum Field: Hashable {
        case host, port
    }

    private var isConnecting: Bool {
        connectionManager.state == .connecting
    }

    private var normalizedHostIP: String {
        hostIP.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var parsedPort: UInt16? {
        UInt16(port.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private var canConnect: Bool {
        !normalizedHostIP.isEmpty && parsedPort != nil
    }

    var body: some View {
        NavigationStack {
            ZStack {
                SignalBackground()

                ScrollView {
                    VStack(spacing: 22) {
                        Spacer().frame(height: 28)

                        logoSection
                            .reveal(appeared, 0)

                        if !didSeeOnboarding {
                            onboardingCard
                                .reveal(appeared, 1)
                        }

                        if let error = connectionManager.connectionError {
                            errorBanner(error)
                        }

                        inputSection
                            .reveal(appeared, 2)

                        if isConnecting || connectionManager.connectionError != nil || !connectionManager.diagnostics.isEmpty {
                            diagnosticsSection
                        }

                        if isConnecting {
                            connectingSection
                        } else {
                            connectButton
                                .reveal(appeared, 3)
                        }

                        HStack(spacing: 12) {
                            scanButton
                            qrScanButton
                        }
                        .reveal(appeared, 4)

                        if scanner.hosts.isEmpty && !scanner.isScanning && !scanner.statusMessage.isEmpty {
                            scanEmptyState
                        }

                        if !scanner.hosts.isEmpty {
                            discoveredSection
                        }

                        if !recentStore.connections.isEmpty && scanner.hosts.isEmpty {
                            recentSection
                                .reveal(appeared, 5)
                        }

                        Spacer().frame(height: 36)
                    }
                    .padding(.horizontal, 28)
                }
                .scrollDismissesKeyboard(.interactively)
            }
            .ignoresSafeArea(.keyboard, edges: .bottom)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showSettings = true
                    } label: {
                        Image(systemName: "slider.horizontal.3")
                            .foregroundColor(Theme.amber)
                    }
                    .accessibilityLabel("Settings")
                }
            }
            .sheet(isPresented: $showSettings) {
                SettingsView()
            }
            .sheet(isPresented: $showQRScanner) {
                QRScannerView(
                    onScan: { value in
                        showQRScanner = false
                        handleScannedQR(value)
                    },
                    onCancel: { showQRScanner = false }
                )
            }
            .onAppear {
                if !didPrefillFromLastHost && hostIP.isEmpty && !settings.lastHost.isEmpty {
                    hostIP = settings.lastHost
                    if settings.lastPort != 0 {
                        port = String(settings.lastPort)
                    }
                    didPrefillFromLastHost = true
                }
                if !appeared {
                    withAnimation(.easeOut(duration: 0.5)) { appeared = true }
                }
            }
        }
        .tint(Theme.amber)
    }

    // MARK: - Logo lockup

    private var logoSection: some View {
        VStack(spacing: 14) {
            ZStack {
                if UIImage(named: "LogoImage") != nil {
                    Image("LogoImage")
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(width: 76, height: 76)
                        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                } else {
                    signalMark
                        .frame(width: 76, height: 76)
                        .background(
                            RoundedRectangle(cornerRadius: 18, style: .continuous)
                                .fill(Theme.amber.opacity(0.10))
                        )
                }
            }
            .overlay(
                ViewfinderCorners(armLength: 12, inset: -6)
                    .stroke(Theme.amber.opacity(0.5), lineWidth: 1)
            )

            VStack(spacing: 3) {
                Text("ETERNALMONITOR")
                    .font(.appDisplayBold(size: 26))
                    .tracking(1)
                    .foregroundColor(Theme.text)

                Text("WINDOWS DISPLAY · SIGNAL LINK")
                    .font(.appMonoRegular(size: 11))
                    .tracking(2)
                    .foregroundColor(Theme.text3)
            }
        }
    }

    // Three ascending amber bars — a transmit-level meter.
    private var signalMark: some View {
        HStack(alignment: .bottom, spacing: 5) {
            ForEach(0..<3, id: \.self) { i in
                RoundedRectangle(cornerRadius: 1)
                    .fill(Theme.amber)
                    .frame(width: 7, height: CGFloat(14 + i * 11))
            }
        }
    }

    // MARK: - Onboarding hint

    private var onboardingCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                SectionLabel(title: "How it works")
                Spacer()
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) { didSeeOnboarding = true }
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundColor(Theme.text3)
                }
                .accessibilityLabel("Dismiss tips")
            }
            onboardingStep("1", "Run EternalMonitor on your Windows PC.")
            onboardingStep("2", "Scan the QR it shows, or type the PC's IP below.")
            onboardingStep("3", "Tap Connect — your screen appears here.")
        }
        .moduleCard()
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    private func onboardingStep(_ n: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Text(n)
                .font(.appMonoMedium(size: 11))
                .foregroundColor(Theme.void)
                .frame(width: 18, height: 18)
                .background(Circle().fill(Theme.amber))
            Text(text)
                .font(.appMonoRegular(size: 13))
                .foregroundColor(Theme.text2)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
    }

    // MARK: - Error banner

    private func errorBanner(_ message: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(Theme.caution)
                .font(.system(size: 16))

            Text(message)
                .font(.appMonoRegular(size: 12))
                .foregroundColor(Theme.text2)
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)

            Spacer(minLength: 0)

            Button {
                withAnimation { connectionManager.connectionError = nil }
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(Theme.text3)
            }
            .accessibilityLabel("Dismiss error")
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Theme.caution.opacity(0.10))
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(Theme.caution.opacity(0.30), lineWidth: 1)
                )
        )
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    // MARK: - Input fields

    private var inputSection: some View {
        VStack(spacing: 14) {
            field(label: "HOST IP", placeholder: "e.g. 10.0.0.45", text: $hostIP, field: .host, keyboard: .decimalPad)

            if !settings.lastHost.isEmpty && settings.lastHost != normalizedHostIP {
                Button {
                    hostIP = settings.lastHost
                    if settings.lastPort != 0 { port = String(settings.lastPort) }
                } label: {
                    Text("Use last: \(settings.lastHost)")
                        .font(.appMonoRegular(size: 11))
                        .foregroundColor(Theme.amber.opacity(0.8))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }

            field(label: "PORT", placeholder: "9876", text: $port, field: .port, keyboard: .numberPad)

            if !port.isEmpty && parsedPort == nil {
                Text("Enter a valid UDP port between 1 and 65535.")
                    .font(.appMonoRegular(size: 11))
                    .foregroundColor(Theme.caution)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .opacity(isConnecting ? 0.5 : 1)
    }

    private func field(label: String, placeholder: String, text: Binding<String>, field: Field, keyboard: UIKeyboardType) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label)
                .font(.appMonoMedium(size: 11))
                .tracking(1)
                .foregroundColor(Theme.text3)

            TextField(placeholder, text: text)
                .font(.appMonoRegular(size: 17))
                .foregroundColor(Theme.text)
                .padding(.horizontal, 16)
                .padding(.vertical, 15)
                .background(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .fill(Color.white.opacity(0.04))
                        .overlay(
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .strokeBorder(
                                    focusedField == field ? Theme.amber : Theme.hairline,
                                    lineWidth: 1
                                )
                        )
                )
                .keyboardType(keyboard)
                .textContentType(.none)
                .autocorrectionDisabled()
                .focused($focusedField, equals: field)
                .disabled(isConnecting)
        }
    }

    // MARK: - Connect button

    private var connectButton: some View {
        Button {
            focusedField = nil
            guard let p = parsedPort else { return }
            withAnimation(.easeInOut(duration: 0.2)) {
                connectionManager.connect(host: normalizedHostIP, port: p)
            }
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "dot.radiowaves.up.forward")
                Text("Connect")
                    .font(.appDisplayBold(size: 17))
            }
        }
        .buttonStyle(AmberButtonStyle())
        .disabled(!canConnect)
        .opacity(canConnect ? 1 : 0.45)
        .accessibilityLabel("Connect to host")
    }

    // MARK: - Diagnostics

    private var diagnosticsSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionLabel(title: "Diagnostics")

            let entries = Array(connectionManager.diagnostics.suffix(8).reversed())
            ForEach(entries) { entry in
                HStack(alignment: .top, spacing: 10) {
                    Text(entry.level.rawValue)
                        .font(.appMonoMedium(size: 10))
                        .foregroundColor(color(for: entry.level))
                        .frame(width: 42, alignment: .leading)

                    VStack(alignment: .leading, spacing: 4) {
                        Text(entry.category.uppercased())
                            .font(.appMonoMedium(size: 10))
                            .foregroundColor(Theme.text3)

                        Text(entry.message)
                            .font(.appMonoRegular(size: 11))
                            .foregroundColor(Theme.text2)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                    }

                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .fill(Color.white.opacity(0.035))
                        .overlay(
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .strokeBorder(Theme.hairline, lineWidth: 1)
                        )
                )
            }
        }
    }

    // MARK: - Connecting state

    private var connectingSection: some View {
        VStack(spacing: 14) {
            HStack(spacing: 12) {
                SignalDot(color: Theme.amber)
                Text("Acquiring signal from \(normalizedHostIP)…")
                    .font(.appMonoRegular(size: 14))
                    .foregroundColor(Theme.text2)
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(16)
            .background(
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .fill(Theme.amber.opacity(0.08))
                    .overlay(
                        RoundedRectangle(cornerRadius: 13, style: .continuous)
                            .strokeBorder(Theme.amber.opacity(0.25), lineWidth: 1)
                    )
            )

            Button {
                withAnimation(.easeInOut(duration: 0.2)) {
                    connectionManager.cancel()
                }
            } label: {
                Text("Cancel")
                    .font(.appDisplayBold(size: 16))
            }
            .buttonStyle(GhostButtonStyle(accent: Theme.amber))
        }
    }

    // MARK: - Scan buttons

    private var scanButton: some View {
        Button {
            focusedField = nil
            if scanner.isScanning { scanner.stopScan() } else { scanner.startScan() }
        } label: {
            HStack(spacing: 8) {
                if scanner.isScanning {
                    ProgressView().tint(Theme.amber).scaleEffect(0.8)
                } else {
                    Image(systemName: "antenna.radiowaves.left.and.right")
                }
                Text(scanner.isScanning ? "Scanning" : "Scan")
                    .font(.appMonoMedium(size: 13))
            }
        }
        .buttonStyle(GhostButtonStyle(accent: scanner.isScanning ? Theme.amber : Theme.text2))
        .disabled(isConnecting)
        .opacity(isConnecting ? 0.5 : 1)
    }

    private var qrScanButton: some View {
        Button {
            focusedField = nil
            showQRScanner = true
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "qrcode.viewfinder")
                Text("Scan QR")
                    .font(.appMonoMedium(size: 13))
            }
        }
        .buttonStyle(GhostButtonStyle(accent: Theme.text2))
        .disabled(isConnecting)
        .opacity(isConnecting ? 0.5 : 1)
    }

    // parse "eternaldisplay://IP:port" and immediately connect.
    private func handleScannedQR(_ value: String) {
        let prefix = "eternaldisplay://"
        guard value.hasPrefix(prefix) else {
            connectionManager.connectionError = "Scanned QR is not an EternalMonitor link."
            return
        }
        let body = String(value.dropFirst(prefix.count))
        let parts = body.split(separator: ":")
        guard parts.count == 2, let scannedPort = UInt16(parts[1]) else {
            connectionManager.connectionError = "Scanned QR is malformed."
            return
        }
        hostIP = String(parts[0])
        port = String(scannedPort)
        connectionManager.connect(host: hostIP, port: scannedPort)
    }

    // MARK: - Scan empty state

    private var scanEmptyState: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "wifi.exclamationmark")
                .foregroundColor(Theme.text3)
                .font(.system(size: 15))
            Text("No hosts found. Enter the IP shown on the PC, and check both devices are on the same Wi-Fi (not a guest network).")
                .font(.appMonoRegular(size: 12))
                .foregroundColor(Theme.text2)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color.white.opacity(0.03))
        )
        .transition(.opacity)
    }

    // MARK: - Discovered hosts

    private var discoveredSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                SectionLabel(title: "Found on network")
                Spacer()
                if !scanner.statusMessage.isEmpty {
                    Text(scanner.statusMessage)
                        .font(.appMonoRegular(size: 10))
                        .foregroundColor(Theme.phosphor.opacity(0.85))
                }
            }

            ForEach(scanner.hosts) { host in
                Button {
                    hostIP = host.address
                    port = "\(host.port)"
                } label: {
                    HStack(spacing: 12) {
                        SignalDot(color: Theme.phosphor, size: 8)

                        VStack(alignment: .leading, spacing: 2) {
                            Text(host.address)
                                .font(.appMonoRegular(size: 15))
                                .foregroundColor(Theme.text)
                            if host.name != host.address {
                                Text(host.name)
                                    .font(.appMonoRegular(size: 11))
                                    .foregroundColor(Theme.text3)
                            }
                        }

                        Spacer()

                        Text(":\(String(host.port))")
                            .font(.appMonoRegular(size: 12))
                            .foregroundColor(Theme.text3)

                        Image(systemName: "arrow.right.circle.fill")
                            .foregroundColor(Theme.amber.opacity(0.7))
                            .font(.system(size: 18))
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 13)
                    .background(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .fill(Theme.phosphor.opacity(0.06))
                            .overlay(
                                RoundedRectangle(cornerRadius: 12, style: .continuous)
                                    .strokeBorder(Theme.phosphor.opacity(0.18), lineWidth: 1)
                            )
                    )
                }
            }
        }
        .transition(.opacity.combined(with: .move(edge: .bottom)))
    }

    // MARK: - Recent connections

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionLabel(title: "Recent")

            ForEach(recentStore.connections) { conn in
                Button {
                    hostIP = conn.host
                    port = "\(conn.port)"
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(conn.host)
                                .font(.appMonoRegular(size: 15))
                                .foregroundColor(Theme.text)
                            Text(":\(String(conn.port))")
                                .font(.appMonoRegular(size: 12))
                                .foregroundColor(Theme.text3)
                        }

                        Spacer()

                        Text(conn.isUSB ? "USB" : "WIFI")
                            .font(.appMonoMedium(size: 10))
                            .tracking(1)
                            .foregroundColor(conn.isUSB ? Theme.amber : Theme.phosphor)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(
                                Capsule().fill((conn.isUSB ? Theme.amber : Theme.phosphor).opacity(0.15))
                            )
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 13)
                    .background(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .fill(Color.white.opacity(0.03))
                    )
                }
            }
        }
        .opacity(isConnecting ? 0.5 : 1)
    }

    private func color(for level: DiagnosticLevel) -> Color {
        switch level {
        case .info: return Theme.phosphor
        case .warning: return Theme.caution
        case .error: return Theme.fault
        }
    }
}

// MARK: - Staggered reveal

private extension View {
    /// Fade + rise on first appear, ordered by `index`.
    func reveal(_ appeared: Bool, _ index: Int) -> some View {
        self
            .opacity(appeared ? 1 : 0)
            .offset(y: appeared ? 0 : 14)
            .animation(.easeOut(duration: 0.45).delay(Double(index) * 0.07), value: appeared)
    }
}
