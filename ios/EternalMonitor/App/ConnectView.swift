import SwiftUI

struct ConnectView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @StateObject private var recentStore = RecentConnectionStore.shared
    @StateObject private var scanner = NetworkScanner()

    @State private var hostIP: String = ""
    @State private var port: String = "9876"
    @State private var showSettings = false
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

    var body: some View {
        NavigationStack {
            ZStack {
                Color(hex: 0x080808).ignoresSafeArea()
                gridOverlay

                ScrollView {
                    VStack(spacing: 28) {
                        Spacer().frame(height: 40)

                        logoSection

                        // Error banner
                        if let error = connectionManager.connectionError {
                            errorBanner(error)
                        }

                        inputSection

                        // Connect / Cancel buttons
                        if isConnecting {
                            connectingSection
                        } else {
                            connectButton
                        }

                        scanButton

                        if !scanner.statusMessage.isEmpty && scanner.hosts.isEmpty {
                            scanStatusMessage
                        }

                        // Discovered hosts
                        if !scanner.hosts.isEmpty {
                            discoveredSection
                        }

                        // Recent connections
                        if !recentStore.connections.isEmpty && scanner.hosts.isEmpty {
                            recentSection
                        }

                        Spacer().frame(height: 40)
                    }
                    .padding(.horizontal, 32)
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showSettings = true
                    } label: {
                        Image(systemName: "gearshape")
                            .foregroundColor(Color(hex: 0xe8ff47))
                    }
                }
            }
            .sheet(isPresented: $showSettings) {
                SettingsView()
            }
        }
    }

    // MARK: - Logo

    private var logoSection: some View {
        VStack(spacing: 12) {
            if UIImage(named: "LogoImage") != nil {
                Image("LogoImage")
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 80, height: 80)
                    .clipShape(RoundedRectangle(cornerRadius: 18))
            } else {
                // Fallback if image asset isn't found
                Text("E")
                    .font(.custom("Syne-Bold", size: 48))
                    .foregroundColor(Color(hex: 0xe8ff47))
                    .frame(width: 80, height: 80)
                    .background(
                        RoundedRectangle(cornerRadius: 18)
                            .fill(Color(hex: 0xe8ff47).opacity(0.1))
                    )
            }

            Text("EternalMonitor")
                .font(.custom("Syne-Bold", size: 28))
                .foregroundColor(.white)

            Text("Windows display streaming")
                .font(.custom("JetBrainsMono-Regular", size: 13))
                .foregroundColor(.white.opacity(0.4))
        }
    }

    // MARK: - Error banner

    private func errorBanner(_ message: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.orange)
                .font(.system(size: 16))

            Text(message)
                .font(.custom("JetBrainsMono-Regular", size: 12))
                .foregroundColor(.white.opacity(0.8))
                .multilineTextAlignment(.leading)

            Spacer()

            Button {
                connectionManager.connectionError = nil
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(.white.opacity(0.4))
            }
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color.orange.opacity(0.1))
                .overlay(
                    RoundedRectangle(cornerRadius: 12)
                        .strokeBorder(Color.orange.opacity(0.3), lineWidth: 1)
                )
        )
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    // MARK: - Input fields

    private var inputSection: some View {
        VStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 6) {
                Text("HOST IP")
                    .font(.custom("JetBrainsMono-Medium", size: 11))
                    .foregroundColor(.white.opacity(0.4))

                TextField("e.g. 10.0.0.45", text: $hostIP)
                    .font(.custom("JetBrainsMono-Regular", size: 16))
                    .foregroundColor(.white)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 14)
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(Color.white.opacity(0.05))
                            .overlay(
                                RoundedRectangle(cornerRadius: 12)
                                    .strokeBorder(
                                        focusedField == .host
                                            ? Color(hex: 0xe8ff47)
                                            : Color.white.opacity(0.1),
                                        lineWidth: 1
                                    )
                            )
                    )
                    .keyboardType(.decimalPad)
                    .textContentType(.none)
                    .autocorrectionDisabled()
                    .focused($focusedField, equals: .host)
                    .disabled(isConnecting)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("PORT")
                    .font(.custom("JetBrainsMono-Medium", size: 11))
                    .foregroundColor(.white.opacity(0.4))

                TextField("9876", text: $port)
                    .font(.custom("JetBrainsMono-Regular", size: 16))
                    .foregroundColor(.white)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 14)
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(Color.white.opacity(0.05))
                            .overlay(
                                RoundedRectangle(cornerRadius: 12)
                                    .strokeBorder(
                                        focusedField == .port
                                            ? Color(hex: 0xe8ff47)
                                            : Color.white.opacity(0.1),
                                        lineWidth: 1
                                    )
                            )
                    )
                    .keyboardType(.numberPad)
                    .focused($focusedField, equals: .port)
                    .disabled(isConnecting)
            }

            if !port.isEmpty && parsedPort == nil {
                Text("Enter a valid UDP port between 1 and 65535.")
                    .font(.custom("JetBrainsMono-Regular", size: 11))
                    .foregroundColor(.orange.opacity(0.8))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .opacity(isConnecting ? 0.5 : 1)
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
                Image(systemName: "link")
                Text("Connect")
                    .font(.custom("Syne-Bold", size: 16))
            }
            .foregroundColor(Color(hex: 0x080808))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16)
            .background(
                RoundedRectangle(cornerRadius: 14)
                    .fill(Color(hex: 0xe8ff47))
            )
        }
        .disabled(normalizedHostIP.isEmpty || parsedPort == nil)
        .opacity(normalizedHostIP.isEmpty || parsedPort == nil ? 0.5 : 1)
    }

    // MARK: - Connecting state (spinner + cancel)

    private var connectingSection: some View {
        VStack(spacing: 14) {
            // Status
            HStack(spacing: 10) {
                ProgressView()
                    .tint(Color(hex: 0xe8ff47))
                Text("Waiting for frames from \(normalizedHostIP)...")
                    .font(.custom("JetBrainsMono-Regular", size: 14))
                    .foregroundColor(.white.opacity(0.7))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16)
            .background(
                RoundedRectangle(cornerRadius: 14)
                    .fill(Color(hex: 0xe8ff47).opacity(0.08))
                    .overlay(
                        RoundedRectangle(cornerRadius: 14)
                            .strokeBorder(Color(hex: 0xe8ff47).opacity(0.2), lineWidth: 1)
                    )
            )

            // Cancel button
            Button {
                withAnimation(.easeInOut(duration: 0.2)) {
                    connectionManager.cancel()
                }
            } label: {
                Text("Cancel")
                    .font(.custom("Syne-Bold", size: 16))
                    .foregroundColor(Color(hex: 0xe8ff47))
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 14)
                    .background(
                        RoundedRectangle(cornerRadius: 14)
                            .strokeBorder(Color(hex: 0xe8ff47).opacity(0.4), lineWidth: 1)
                    )
            }
        }
    }

    // MARK: - Scan network

    private var scanButton: some View {
        Button {
            focusedField = nil
            if scanner.isScanning {
                scanner.stopScan()
            } else {
                scanner.startScan()
            }
        } label: {
            HStack(spacing: 8) {
                if scanner.isScanning {
                    ProgressView()
                        .tint(.white.opacity(0.5))
                        .scaleEffect(0.8)
                } else {
                    Image(systemName: "antenna.radiowaves.left.and.right")
                }
                Text(scanner.isScanning ? "Scanning..." : "Scan network")
                    .font(.custom("JetBrainsMono-Regular", size: 14))
            }
            .foregroundColor(.white.opacity(0.6))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 14)
            .background(
                RoundedRectangle(cornerRadius: 14)
                    .strokeBorder(
                        scanner.isScanning
                            ? Color(hex: 0xe8ff47).opacity(0.3)
                            : Color.white.opacity(0.1),
                        lineWidth: 1
                    )
            )
        }
        .disabled(isConnecting)
        .opacity(isConnecting ? 0.5 : 1)
    }

    // MARK: - Discovered hosts

    private var discoveredSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("FOUND ON NETWORK")
                    .font(.custom("JetBrainsMono-Medium", size: 11))
                    .foregroundColor(.white.opacity(0.4))

                Spacer()

                if !scanner.statusMessage.isEmpty {
                    Text(scanner.statusMessage)
                        .font(.custom("JetBrainsMono-Regular", size: 10))
                        .foregroundColor(Color(hex: 0x1D9E75).opacity(0.8))
                }
            }

            ForEach(scanner.hosts) { host in
                Button {
                    hostIP = host.address
                    port = "\(host.port)"
                } label: {
                    HStack(spacing: 12) {
                        Circle()
                            .fill(Color(hex: 0x1D9E75))
                            .frame(width: 8, height: 8)

                        VStack(alignment: .leading, spacing: 2) {
                            Text(host.address)
                                .font(.custom("JetBrainsMono-Regular", size: 15))
                                .foregroundColor(.white)
                            if host.name != host.address {
                                Text(host.name)
                                    .font(.custom("JetBrainsMono-Regular", size: 11))
                                    .foregroundColor(.white.opacity(0.3))
                            }
                        }

                        Spacer()

                        Text(":\(String(host.port))")
                            .font(.custom("JetBrainsMono-Regular", size: 12))
                            .foregroundColor(.white.opacity(0.3))

                        Image(systemName: "arrow.right.circle.fill")
                            .foregroundColor(Color(hex: 0xe8ff47).opacity(0.6))
                            .font(.system(size: 18))
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(Color(hex: 0x1D9E75).opacity(0.06))
                            .overlay(
                                RoundedRectangle(cornerRadius: 12)
                                    .strokeBorder(Color(hex: 0x1D9E75).opacity(0.15), lineWidth: 1)
                            )
                    )
                }
            }
        }
        .transition(.opacity.combined(with: .move(edge: .bottom)))
    }

    private var scanStatusMessage: some View {
        Text(scanner.statusMessage)
            .font(.custom("JetBrainsMono-Regular", size: 11))
            .foregroundColor(.white.opacity(0.45))
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Recent connections

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("RECENT")
                .font(.custom("JetBrainsMono-Medium", size: 11))
                .foregroundColor(.white.opacity(0.4))

            ForEach(recentStore.connections) { conn in
                Button {
                    hostIP = conn.host
                    port = "\(conn.port)"
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(conn.host)
                                .font(.custom("JetBrainsMono-Regular", size: 15))
                                .foregroundColor(.white)
                            Text(":\(String(conn.port))")
                                .font(.custom("JetBrainsMono-Regular", size: 12))
                                .foregroundColor(.white.opacity(0.3))
                        }

                        Spacer()

                        Text(conn.isUSB ? "USB" : "WiFi")
                            .font(.custom("JetBrainsMono-Medium", size: 10))
                            .foregroundColor(conn.isUSB ? Color(hex: 0xe8ff47) : Color(hex: 0x1D9E75))
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(
                                Capsule()
                                    .fill(
                                        (conn.isUSB ? Color(hex: 0xe8ff47) : Color(hex: 0x1D9E75))
                                            .opacity(0.15)
                                    )
                            )
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(Color.white.opacity(0.03))
                    )
                }
            }
        }
        .opacity(isConnecting ? 0.5 : 1)
    }

    // MARK: - Grid overlay

    private var gridOverlay: some View {
        Canvas { context, size in
            let spacing: CGFloat = 40
            let color = Color.white.opacity(0.03)

            var x: CGFloat = 0
            while x < size.width {
                context.stroke(
                    Path { path in
                        path.move(to: CGPoint(x: x, y: 0))
                        path.addLine(to: CGPoint(x: x, y: size.height))
                    },
                    with: .color(color),
                    lineWidth: 0.5
                )
                x += spacing
            }

            var y: CGFloat = 0
            while y < size.height {
                context.stroke(
                    Path { path in
                        path.move(to: CGPoint(x: 0, y: y))
                        path.addLine(to: CGPoint(x: size.width, y: y))
                    },
                    with: .color(color),
                    lineWidth: 0.5
                )
                y += spacing
            }
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
    }
}

// MARK: - Color hex extension

extension Color {
    init(hex: UInt32) {
        let r = Double((hex >> 16) & 0xFF) / 255.0
        let g = Double((hex >> 8) & 0xFF) / 255.0
        let b = Double(hex & 0xFF) / 255.0
        self.init(red: r, green: g, blue: b)
    }
}
