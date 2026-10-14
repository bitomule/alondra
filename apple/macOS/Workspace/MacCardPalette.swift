import AlondraContract
import AppKit
import SwiftUI

enum MacCardPalette {
    static func kind(_ kind: CardKind?) -> Color {
        switch kind {
        case .problem: adaptive(light: 0x8F6B12, dark: 0xD6B35A)
        case .authorize: adaptive(light: 0xB8394D, dark: 0xD9697A)
        case .do: adaptive(light: 0x2F6FA8, dark: 0x6EA3D8)
        case .decide: adaptive(light: 0xB5601F, dark: 0xE8935A)
        case .review, .test: adaptive(light: 0x3F7A55, dark: 0x7FA98B)
        case .provide: adaptive(light: 0x7A5BB5, dark: 0xA58BD6)
        case .propose: adaptive(light: 0x7A746A, dark: 0xC9C3BA)
        case .know, nil: adaptive(light: 0x6B717C, dark: 0x9AA0AA)
        }
    }

    static let advance = adaptive(light: 0x2F7D4A, dark: 0x5E9C70)
    static let onAdvance = adaptive(light: 0xFFFFFF, dark: 0x0F1612)
    static let risk = adaptive(light: 0xB8394D, dark: 0xD9697A)
    static let onRisk = adaptive(light: 0xFFFFFF, dark: 0x1A0E10)
    static let neutralBorder = adaptive(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.22, darkAlpha: 0.26)
    static let surface = adaptive(light: 0xFFFDFA, dark: 0x1C1E27)
    static let hairline = adaptive(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.09, darkAlpha: 0.10)
    static let inset = adaptive(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.04, darkAlpha: 0.06)

    static func tone(_ tone: Tone) -> Color {
        switch tone {
        case .neutral: .secondary
        case .info: adaptive(light: 0x2F6FA8, dark: 0x6EA3D8)
        case .accent: adaptive(light: 0xB5601F, dark: 0xE8935A)
        case .success: adaptive(light: 0x3F7A55, dark: 0x7FA98B)
        case .warning, .yourTurn: adaptive(light: 0xB5601F, dark: 0xE8935A)
        case .critical: adaptive(light: 0xB8394D, dark: 0xD9697A)
        }
    }

    private static func adaptive(light: UInt32, dark: UInt32, lightAlpha: Double = 1, darkAlpha: Double = 1) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            let hex = isDark ? dark : light
            return NSColor(
                srgbRed: CGFloat((hex >> 16) & 255) / 255, green: CGFloat((hex >> 8) & 255) / 255,
                blue: CGFloat(hex & 255) / 255, alpha: isDark ? darkAlpha : lightAlpha)
        })
    }
}
