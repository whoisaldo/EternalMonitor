import SwiftUI

struct ConnectView: View {
    @EnvironmentObject var connectionManager: ConnectionManager
    @StateObject private var recentStore = RecentConnectionStore.shared

    @State private var hostIP: String = ""
    @State private var port: String = "9876"
    @State private var showSettings = false
    @FocusState private var focusedField: Field?

    private enum Field: Hashable {
        case host, port
    }

    var body: some View {
        ZStack {
            // Background
            Color(hex: 0x080808).ignoresSafeArea()
            gridOverlay

            ScrollView {
                VStack(spacing: 32) {
                    Spacer().frame(height: 60)

                    // Logo + Title
                    logoSection

                    // Input fields
                    inputSection

                    // Connect button
                    connectButton

                    // Scan network (placeholder)
                    scanButton

                    // Recent connections
                    if !recentStore.connections.isEmpty {
                        recentSection
                    }

                    Spacer()
                }
                .padding(.horizontal, 32)
            }
        }
        .sheet(isPresented: $showSettings) {
            SettingsView()
        }
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
    }

    // MARK: - Logo

    private var logoSection: some View {
        VStack(spacing: 12) {
            // App logo
            Image("LogoImage")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 88, height: 88)
                .clipShape(RoundedRectangle(cornerRadius: 20))

            Text("EternalMonitor")
                .font(.custom("Syne-Bold", size: 32))
                .foregroundColor(.white)

            Text("Windows display streaming")
                .font(.custom("JetBrainsMono-Regular", size: 14))
                .foregroundColor(.white.opacity(0.5))
        }
    }

    // MARK: - Input fields

    private var inputSection: some View {
        VStack(spacing: 16) {
            // Host IP
            VStack(alignment: .leading, spacing: 6) {
                Text("HOST IP")
                    .font(.custom("JetBrainsMono-Medium", size: 11))
                    .foregroundColor(.white.opacity(0.4))

                TextField("192.168.1.100", text: $hostIP)
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
            }

            // Port
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
            }
        }
    }

    // MARK: - Buttons

    private var connectButton: some View {
        Button {
            let p = UInt16(port) ?? 9876
            connectionManager.connect(host: hostIP, port: p)
        } label: {
            HStack(spacing: 8) {
                if connectionManager.state == .connecting {
                    ProgressView()
                        .tint(Color(hex: 0x080808))
                } else {
                    Image(systemName: "link")
                }
                Text(connectionManager.state == .connecting ? "Connecting..." : "Connect")
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
        .disabled(hostIP.isEmpty || connectionManager.state == .connecting)
        .opacity(hostIP.isEmpty ? 0.5 : 1)
    }

    private var scanButton: some View {
        Button {
            // Placeholder — mDNS discovery is Phase 8
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "antenna.radiowaves.left.and.right")
                Text("Scan network")
                    .font(.custom("JetBrainsMono-Regular", size: 14))
            }
            .foregroundColor(.white.opacity(0.5))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 14)
            .background(
                RoundedRectangle(cornerRadius: 14)
                    .strokeBorder(Color.white.opacity(0.1), lineWidth: 1)
            )
        }
    }

    // MARK: - Recent connections

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 12) {
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
                            Text(":\(conn.port)")
                                .font(.custom("JetBrainsMono-Regular", size: 12))
                                .foregroundColor(.white.opacity(0.3))
                        }

                        Spacer()

                        // Transport badge
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
    }

    // MARK: - Grid overlay

    private var gridOverlay: some View {
        Canvas { context, size in
            let spacing: CGFloat = 40
            let color = Color.white.opacity(0.03)

            // Vertical lines
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

            // Horizontal lines
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
