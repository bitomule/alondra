import AlondraShared
import SwiftUI
import UIKit

enum Palette {
    static let dawn = dynamic(light: 0xD9661F, dark: 0xFF9A4D)
    static let ember = dynamic(light: 0xC2410C, dark: 0xFF7A45)
    static let sky = dynamic(light: 0x2F6FB0, dark: 0x7FB6F0)
    static let sage = dynamic(light: 0x3F7D4E, dark: 0x8CCB94)
    static let rose = dynamic(light: 0xB83A4B, dark: 0xFF8A9A)
    static let slate = dynamic(light: 0x6B6F7A, dark: 0xA6ABB8)

    static let night = dynamic(light: 0xF6F1EA, dark: 0x0C0D12)
    static let dusk = dynamic(light: 0xEDE4D8, dark: 0x171922)
    static let card = dynamic(light: 0xFFFDFA, dark: 0x1C1E27)
    static let cardRaised = dynamic(light: 0xFFFFFF, dark: 0x252836)
    static let hairline = dynamic(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.08, darkAlpha: 0.09)
    static let ink = dynamic(light: 0x1B1A18, dark: 0xF4EFE8)
    static let onAccent = dynamic(light: 0xFFFFFF, dark: 0x111217)
    static let sageDeep = dynamic(light: 0x3F7D4E, dark: 0x3E8A51)
    static let skyDeep = dynamic(light: 0x2F6FB0, dark: 0x3576BD)

    private static func dynamic(light: UInt32, dark: UInt32, lightAlpha: CGFloat = 1, darkAlpha: CGFloat = 1) -> Color {
        Color(uiColor: UIColor { traits in
            traits.userInterfaceStyle == .dark ? UIColor(hex: dark, alpha: darkAlpha) : UIColor(hex: light, alpha: lightAlpha)
        })
    }
}

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

struct DawnBackground: View {
    var glow: Color = Palette.dawn

    var body: some View {
        ZStack {
            LinearGradient(colors: [Palette.dusk, Palette.night], startPoint: .top, endPoint: .center)
            RadialGradient(colors: [glow.opacity(0.28), glow.opacity(0.06), .clear], center: .init(x: 0.85, y: -0.05), startRadius: 10, endRadius: 520)
            RadialGradient(colors: [Palette.sky.opacity(0.10), .clear], center: .init(x: 0.0, y: 0.25), startRadius: 10, endRadius: 380)
        }
        .ignoresSafeArea()
    }
}

extension Font {
    static func display(_ style: Font.TextStyle, weight: Font.Weight = .semibold) -> Font {
        .system(style, design: .serif, weight: weight)
    }

    static func mono(_ style: Font.TextStyle, weight: Font.Weight = .regular) -> Font {
        .system(style, design: .monospaced, weight: weight)
    }
}

struct Kind {
    let label: String
    let symbol: String
    let tint: Color

    init(_ item: InboxItem) {
        switch item {
        case .question(let q):
            switch q.origin {
            case "plan": self.init("Plan", "list.bullet.clipboard", Palette.ember)
            case "permission": self.init("Permiso", "lock.shield", Palette.rose)
            default: self.init("Pregunta", "questionmark.bubble", Palette.dawn)
            }
        case .report(let r):
            switch r.kind {
            case "document": self.init("Documento", "doc.richtext", Palette.sky)
            case "final": self.init("Terminado", "flag.checkered", Palette.sage)
            case "notice": self.init("Aviso", "bell", Palette.slate)
            default: self.init("Progreso", "chart.line.uptrend.xyaxis", Palette.slate)
            }
        }
    }

    private init(_ label: String, _ symbol: String, _ tint: Color) {
        self.label = label
        self.symbol = symbol
        self.tint = tint
    }
}

extension NodeState {
    var tint: Color {
        switch self {
        case .working: Palette.sky
        case .waitingAnswer: Palette.dawn
        case .delivered: Palette.sage
        case .waitingMachine: Palette.slate
        case .done: Palette.sage
        case .unclear: Palette.rose
        }
    }

    var symbol: String {
        switch self {
        case .working: "hammer.fill"
        case .waitingAnswer: "hand.raised.fill"
        case .delivered: "tray.and.arrow.down.fill"
        case .waitingMachine: "hourglass"
        case .done: "checkmark"
        case .unclear: "questionmark"
        }
    }
}

struct KindBadge: View {
    let kind: Kind

    var body: some View {
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
    init(millis: Int64) {
        self.init(timeIntervalSince1970: TimeInterval(millis) / 1000)
    }
}

func relative(_ millis: Int64) -> String {
    let date = Date(millis: millis)
    if abs(date.timeIntervalSinceNow) < 45 { return "ahora" }
    return date.formatted(.relative(presentation: .named, unitsStyle: .abbreviated))
}
