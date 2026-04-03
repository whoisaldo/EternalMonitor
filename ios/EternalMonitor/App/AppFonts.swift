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
