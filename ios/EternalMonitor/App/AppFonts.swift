import SwiftUI
import CoreText
import CoreFoundation
import os

enum AppFontStyle {
    case monoRegular
    case monoMedium
    case displayBold

    var postScriptName: String {
        switch self {
        case .monoRegular:
            return "JetBrainsMono-Regular"
        case .monoMedium:
            return "JetBrainsMono-Medium"
        case .displayBold:
            return "Syne-Bold"
        }
    }

    func font(size: CGFloat) -> Font {
        if FontRegistry.shared.availablePostScriptNames.contains(postScriptName) {
            Font.custom(postScriptName, size: size)
        } else {
            switch self {
            case .monoRegular:
                Font.system(size: size, weight: .regular, design: .monospaced)
            case .monoMedium:
                Font.system(size: size, weight: .medium, design: .monospaced)
            case .displayBold:
                Font.system(size: size, weight: .bold, design: .default)
            }
        }
    }
}

final class FontRegistry: @unchecked Sendable {
    static let shared = FontRegistry()

    private(set) var availablePostScriptNames: Set<String> = []
    private let logger = Logger(subsystem: "com.eternal.monitor", category: "fonts")
    private var didRegister = false

    private init() {}

    func registerBundledFonts() {
        guard !didRegister else { return }
        didRegister = true

        let candidates = bundledFontURLs()
        if candidates.isEmpty {
            logger.error("No bundled font files were found in the app bundle")
            return
        }

        for url in candidates {
            var error: Unmanaged<CFError>?
            if CTFontManagerRegisterFontsForURL(url as CFURL, .process, &error) {
                logger.info("Registered bundled font at \(url.lastPathComponent, privacy: .public)")
            } else if let error {
                let resolvedError = error.takeRetainedValue()
                let code = CFErrorGetCode(resolvedError)
                if code == CTFontManagerError.alreadyRegistered.rawValue {
                    logger.info("Font already registered at \(url.lastPathComponent, privacy: .public)")
                } else {
                    logger.error("Font registration failed for \(url.lastPathComponent, privacy: .public): \(resolvedError.localizedDescription, privacy: .public)")
                }
            }

            if let descriptors = CTFontManagerCreateFontDescriptorsFromURL(url as CFURL) as? [CTFontDescriptor] {
                for descriptor in descriptors {
                    if let name = CTFontDescriptorCopyAttribute(descriptor, kCTFontNameAttribute) as? String {
                        availablePostScriptNames.insert(name)
                    }
                }
            }
        }

        let fontList = self.availablePostScriptNames.sorted().joined(separator: ", ")
        logger.info("Available app fonts: \(fontList, privacy: .public)")
    }

    private func bundledFontURLs() -> [URL] {
        let rootNames = [
            "Syne-Variable.ttf",
            "JetBrainsMono-Regular.ttf",
            "JetBrainsMono-Medium.ttf",
        ]

        let bundle = Bundle.main
        let rootURLs = rootNames.compactMap { bundle.url(forResource: $0.replacingOccurrences(of: ".ttf", with: ""), withExtension: "ttf") }
        let nestedURLs = rootNames.compactMap { bundle.url(forResource: $0.replacingOccurrences(of: ".ttf", with: ""), withExtension: "ttf", subdirectory: "Fonts") }

        let all = rootURLs + nestedURLs
        if all.isEmpty {
            logger.error("Bundle did not resolve any expected font paths")
        } else {
            for url in all {
                logger.info("Discovered bundled font candidate at \(url.path(percentEncoded: false), privacy: .public)")
            }
        }
        return all
    }
}

extension Font {
    static func appMonoRegular(size: CGFloat) -> Font {
        AppFontStyle.monoRegular.font(size: size)
    }

    static func appMonoMedium(size: CGFloat) -> Font {
        AppFontStyle.monoMedium.font(size: size)
    }

    static func appDisplayBold(size: CGFloat) -> Font {
        AppFontStyle.displayBold.font(size: size)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// MARK: - EternalMonitor // SIGNAL design system
//
// A broadcast-instrument control surface: void-black, hairline-framed modules,
// "transmit amber" as the brand/action color, "phosphor" mint for live/healthy
// signal, caution-yellow and fault-coral for states. Shared with the Windows
// host so the two halves of the product read as one piece of equipment.
// ════════════════════════════════════════════════════════════════════════════

extension Color {
    /// Build a Color from a 0xRRGGBB literal. Single definition for the whole app.
    init(hex: UInt32) {
        let r = Double((hex >> 16) & 0xFF) / 255.0
        let g = Double((hex >> 8) & 0xFF) / 255.0
        let b = Double(hex & 0xFF) / 255.0
        self.init(red: r, green: g, blue: b)
    }
}

enum Theme {
    static let void = Color(hex: 0x060708)
    static let panel = Color(hex: 0x0F1012)
    static let panel2 = Color(hex: 0x17191C)
    static let hairline = Color.white.opacity(0.08)
    static let hairlineStrong = Color.white.opacity(0.14)

    static let amber = Color(hex: 0xFF7A1A)       // brand / primary action
    static let amberBright = Color(hex: 0xFFB35C)
    static let phosphor = Color(hex: 0x3EE5A6)    // live / healthy signal
    static let caution = Color(hex: 0xFFD23F)     // warning
    static let fault = Color(hex: 0xFF4D5E)       // error / loss

    static let text = Color.white
    static let text2 = Color.white.opacity(0.6)
    static let text3 = Color.white.opacity(0.35)

    /// Phosphor → caution → fault ramp for a 0–4 signal-bar count.
    static func quality(bars: Int) -> Color {
        switch bars {
        case 4: return phosphor
        case 3: return Color(hex: 0xB7E84A)
        case 2: return caution
        default: return fault
        }
    }
}

// MARK: - Viewfinder registration marks

/// Four L-shaped corner ticks just inside the frame — the recurring "this is a
/// monitor" motif. Stroke it as an overlay.
struct ViewfinderCorners: Shape {
    var armLength: CGFloat = 12
    var inset: CGFloat = 4

    func path(in rect: CGRect) -> Path {
        var p = Path()
        let l = rect.minX + inset, r = rect.maxX - inset
        let t = rect.minY + inset, b = rect.maxY - inset
        let a = armLength
        // top-left
        p.move(to: CGPoint(x: l, y: t + a)); p.addLine(to: CGPoint(x: l, y: t)); p.addLine(to: CGPoint(x: l + a, y: t))
        // top-right
        p.move(to: CGPoint(x: r - a, y: t)); p.addLine(to: CGPoint(x: r, y: t)); p.addLine(to: CGPoint(x: r, y: t + a))
        // bottom-left
        p.move(to: CGPoint(x: l, y: b - a)); p.addLine(to: CGPoint(x: l, y: b)); p.addLine(to: CGPoint(x: l + a, y: b))
        // bottom-right
        p.move(to: CGPoint(x: r - a, y: b)); p.addLine(to: CGPoint(x: r, y: b)); p.addLine(to: CGPoint(x: r, y: b - a))
        return p
    }
}

// MARK: - Module card

struct ModuleCard: ViewModifier {
    var tint: Color? = nil
    var corners: Bool = false

    func body(content: Content) -> some View {
        content
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(tint.map { $0.opacity(0.10) } ?? Theme.panel)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(tint.map { $0.opacity(0.35) } ?? Theme.hairline, lineWidth: 1)
            )
            .overlay(
                Group {
                    if corners {
                        ViewfinderCorners(armLength: 10, inset: 7)
                            .stroke(tint ?? Theme.amber, lineWidth: 1)
                            .padding(2)
                    }
                }
            )
    }
}

extension View {
    func moduleCard(tint: Color? = nil, corners: Bool = false) -> some View {
        modifier(ModuleCard(tint: tint, corners: corners))
    }
}

// MARK: - Section header (amber tick + tracked mono label)

struct SectionLabel: View {
    let title: String
    var body: some View {
        HStack(spacing: 7) {
            Rectangle().fill(Theme.amber).frame(width: 3, height: 11)
            Text(title.uppercased())
                .font(.appMonoMedium(size: 11))
                .tracking(1.5)
                .foregroundColor(Theme.text2)
        }
    }
}

// MARK: - Equipment readout

struct Readout: View {
    let value: String
    let unit: String
    var label: String = ""
    var color: Color = Theme.amber

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !label.isEmpty {
                Text(label.uppercased())
                    .font(.appMonoMedium(size: 10))
                    .tracking(1)
                    .foregroundColor(Theme.text3)
            }
            HStack(alignment: .firstTextBaseline, spacing: 3) {
                Text(value)
                    .font(.appMonoMedium(size: 24))
                    .foregroundColor(color)
                if !unit.isEmpty {
                    Text(unit)
                        .font(.appMonoRegular(size: 11))
                        .foregroundColor(Theme.text2)
                }
            }
        }
    }
}

// MARK: - Pulsing signal dot

struct SignalDot: View {
    var color: Color = Theme.phosphor
    var size: CGFloat = 8

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0)) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            let pulse = 0.5 + 0.5 * sin(t * 3.2)
            Circle()
                .fill(color)
                .frame(width: size, height: size)
                .shadow(color: color.opacity(0.7 * pulse), radius: CGFloat(3.0 + 4.0 * pulse))
                .opacity(0.55 + 0.45 * pulse)
        }
    }
}

// MARK: - Signal background (grid + scan sweep + vignette)

struct SignalBackground: View {
    var body: some View {
        ZStack {
            Theme.void

            // Fine reference grid
            Canvas { context, size in
                let spacing: CGFloat = 44
                let color = Color.white.opacity(0.025)
                var x: CGFloat = 0
                while x < size.width {
                    context.stroke(
                        Path { $0.move(to: CGPoint(x: x, y: 0)); $0.addLine(to: CGPoint(x: x, y: size.height)) },
                        with: .color(color), lineWidth: 0.5
                    )
                    x += spacing
                }
                var y: CGFloat = 0
                while y < size.height {
                    context.stroke(
                        Path { $0.move(to: CGPoint(x: 0, y: y)); $0.addLine(to: CGPoint(x: size.width, y: y)) },
                        with: .color(color), lineWidth: 0.5
                    )
                    y += spacing
                }
            }

            // Slow amber scan sweep travelling down the screen
            TimelineView(.animation(minimumInterval: 1.0 / 30.0)) { context in
                GeometryReader { geo in
                    let period = 6.0
                    let t = context.date.timeIntervalSinceReferenceDate
                    let phase = (t.truncatingRemainder(dividingBy: period)) / period
                    let y = geo.size.height * CGFloat(phase)
                    LinearGradient(
                        colors: [.clear, Theme.amber.opacity(0.06), .clear],
                        startPoint: .top, endPoint: .bottom
                    )
                    .frame(height: 160)
                    .offset(y: y - 80)
                }
            }

            // Vignette to focus the center
            RadialGradient(
                colors: [.clear, Theme.void.opacity(0.55)],
                center: .center, startRadius: 200, endRadius: 620
            )
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
    }
}

// MARK: - Button styles

struct AmberButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16)
            .background(
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .fill(Theme.amber)
            )
            .foregroundColor(Theme.void)
            .opacity(configuration.isPressed ? 0.85 : 1)
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

struct GhostButtonStyle: ButtonStyle {
    var accent: Color = Theme.text2
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(maxWidth: .infinity)
            .padding(.vertical, 14)
            .background(
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .strokeBorder(accent.opacity(0.28), lineWidth: 1)
            )
            .foregroundColor(accent)
            .opacity(configuration.isPressed ? 0.55 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}
