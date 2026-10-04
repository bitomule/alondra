import AlondraDesign
import SwiftUI

/// A titled card on a detail screen.
struct DetailSection<Content: View>: View {
    let title: String
    let symbol: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(title, systemImage: symbol)
                .font(.footnote.weight(.bold))
                .textCase(.uppercase)
                .kerning(0.6)
                .foregroundStyle(.secondary)
            content
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
        }
    }
}

struct Hairline: View {
    var leading: CGFloat = 16

    var body: some View {
        Palette.hairline.frame(height: 1).padding(.leading, leading)
    }
}

struct ActionButton: View {
    let title: String
    let symbol: String
    let tint: Color
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: symbol)
                .font(.body.weight(.semibold))
                .frame(maxWidth: .infinity, minHeight: 50)
                .foregroundStyle(tint)
                .background(tint.opacity(0.12), in: .rect(cornerRadius: 18, style: .continuous))
                .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
    }
}
