import AlondraContract
import AlondraFeatures
import SwiftUI

struct MacCardButtonStyle: ButtonStyle {
    let role: ActionRole
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: 10, style: .continuous)
        configuration.label
            .foregroundStyle(isEnabled ? foreground : Color.secondary)
            .background(fill.opacity(isEnabled ? 1 : 0.3), in: shape)
            .overlay(shape.strokeBorder(role == .neutral ? MacCardPalette.neutralBorder : .clear))
            .contentShape(shape)
            .opacity(configuration.isPressed ? 0.8 : 1)
    }

    private var fill: Color {
        switch role {
        case .advance: MacCardPalette.advance
        case .risk: MacCardPalette.risk
        case .neutral: .clear
        }
    }

    private var foreground: Color {
        switch role {
        case .advance: MacCardPalette.onAdvance
        case .risk: MacCardPalette.onRisk
        case .neutral: .primary
        }
    }
}
