import AlondraShared
import SwiftUI

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
