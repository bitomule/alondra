import AlondraContract
import SwiftUI
#if canImport(UIKit)
import UIKit
#else
import AppKit
#endif

public enum Palette {
    public static let dawn = dynamic(light: 0xD9661F, dark: 0xFF9A4D)
    public static let ember = dynamic(light: 0xC2410C, dark: 0xFF7A45)
    public static let sky = dynamic(light: 0x2F6FB0, dark: 0x7FB6F0)
    public static let sage = dynamic(light: 0x3F7D4E, dark: 0x8CCB94)
    public static let rose = dynamic(light: 0xB83A4B, dark: 0xFF8A9A)
    public static let slate = dynamic(light: 0x6B6F7A, dark: 0xA6ABB8)

    public static let night = dynamic(light: 0xF6F1EA, dark: 0x0C0D12)
    public static let dusk = dynamic(light: 0xEDE4D8, dark: 0x171922)
    public static let card = dynamic(light: 0xFFFDFA, dark: 0x1C1E27)
    public static let cardRaised = dynamic(light: 0xFFFFFF, dark: 0x252836)
    public static let hairline = dynamic(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.08, darkAlpha: 0.09)
    public static let ink = dynamic(light: 0x1B1A18, dark: 0xF4EFE8)
    public static let onAccent = dynamic(light: 0xFFFFFF, dark: 0x111217)
    public static let sageDeep = dynamic(light: 0x3F7D4E, dark: 0x3E8A51)
    public static let skyDeep = dynamic(light: 0x2F6FB0, dark: 0x3576BD)

    private static func dynamic(light: UInt32, dark: UInt32, lightAlpha: CGFloat = 1, darkAlpha: CGFloat = 1) -> Color {
        #if canImport(UIKit)
        Color(uiColor: UIColor { traits in
            traits.userInterfaceStyle == .dark ? UIColor(hex: dark, alpha: darkAlpha) : UIColor(hex: light, alpha: lightAlpha)
        })
        #else
        Color(nsColor: NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(hex: dark, alpha: darkAlpha) : NSColor(hex: light, alpha: lightAlpha)
        })
        #endif
    }
}

#if canImport(UIKit)
private extension UIColor {
    convenience init(hex: UInt32, alpha: CGFloat) {
        self.init(
            red: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: alpha
        )
    }
}
#else
private extension NSColor {
    convenience init(hex: UInt32, alpha: CGFloat) {
        self.init(
            srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: alpha
        )
    }
}
#endif

public struct DawnBackground: View {
    public var glow: Color

    public init(glow: Color = Palette.dawn) { self.glow = glow }

    public var body: some View {
        ZStack {
            LinearGradient(colors: [Palette.dusk, Palette.night], startPoint: .top, endPoint: .center)
            RadialGradient(colors: [glow.opacity(0.28), glow.opacity(0.06), .clear], center: .init(x: 0.85, y: -0.05), startRadius: 10, endRadius: 520)
            RadialGradient(colors: [Palette.sky.opacity(0.10), .clear], center: .init(x: 0.0, y: 0.25), startRadius: 10, endRadius: 380)
        }
        .ignoresSafeArea()
    }
}

extension Font {
    public static func display(_ style: Font.TextStyle, weight: Font.Weight = .semibold) -> Font {
        .system(style, design: .serif, weight: weight)
    }

    public static func mono(_ style: Font.TextStyle, weight: Font.Weight = .regular) -> Font {
        .system(style, design: .monospaced, weight: weight)
    }
}

public struct Kind {
    public let label: String
    public let symbol: String
    public let tint: Color

    public init(_ card: AlondraContract.Card) { self.init(icon: card.icon) }

    public init(icon: AlondraContract.Icon) {
        switch icon {
        case .action: self.init(String(localized: "Your turn"), "hand.point.right", Palette.ember)
        case .question: self.init(String(localized: "Question"), "questionmark.bubble", Palette.dawn)
        case .pullRequest: self.init(String(localized: "Pull request"), "arrow.triangle.pull", Palette.sage)
        case .document: self.init(String(localized: "Document"), "doc.richtext", Palette.sky)
        case .delivery: self.init(String(localized: "Finished"), "flag.checkered", Palette.sage)
        case .warning: self.init(String(localized: "Permission"), "lock.shield", Palette.rose)
        default: self.init(String(localized: "Notice"), "bell", Palette.slate)
        }
    }

    private init(_ label: String, _ symbol: String, _ tint: Color) {
        self.label = label
        self.symbol = symbol
        self.tint = tint
    }
}

/// A state's symbol in a tinted circle, at the sizes the lists and cards use.
public struct Glyph: View {
    public enum Size {
        case regular, small, tiny

        var side: CGFloat {
            switch self {
            case .regular: 34
            case .small: 22
            case .tiny: 18
            }
        }

        var font: Font {
            switch self {
            case .regular: .footnote.weight(.bold)
            case .small: .caption2.weight(.bold)
            case .tiny: .system(size: 9, weight: .bold)
            }
        }
    }

    public let symbol: String
    public let tint: Color
    public var size: Size
    public var pulse: Bool

    public init(symbol: String, tint: Color, size: Size = .regular, pulse: Bool = false) {
        self.symbol = symbol
        self.tint = tint
        self.size = size
        self.pulse = pulse
    }

    public var body: some View {
        Image(systemName: symbol)
            .font(size.font)
            .foregroundStyle(tint)
            .frame(width: size.side, height: size.side)
            .background(tint.opacity(0.15), in: .circle)
            .symbolEffect(.pulse, options: .repeating, isActive: pulse)
            .accessibilityHidden(true)
    }
}

public struct KindBadge: View {
    public let kind: Kind

    public init(kind: Kind) { self.kind = kind }

    public var body: some View {
        Label(kind.label, systemImage: kind.symbol)
            .font(.caption.weight(.semibold))
            .textCase(.uppercase)
            .kerning(0.6)
            .foregroundStyle(kind.tint)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(kind.tint.opacity(0.14), in: .capsule)
    }
}

extension Date {
    public init(millis: Int64) {
        self.init(timeIntervalSince1970: TimeInterval(millis) / 1000)
    }
}

/// The instant "2 hr. ago" is measured from. Snapshot tests pin it so a baseline does not age.
public enum RelativeClock {
    public nonisolated(unsafe) static var now: @Sendable () -> Date = { Date() }
}

public func relative(_ millis: Int64) -> String {
    let date = Date(millis: millis)
    let now = RelativeClock.now()
    if abs(date.timeIntervalSince(now)) < 45 { return String(localized: "now") }
    let formatter = RelativeDateTimeFormatter()
    formatter.dateTimeStyle = .named
    formatter.unitsStyle = .abbreviated
    return formatter.localizedString(for: date, relativeTo: now)
}
