import AlondraDesign
import SwiftUI

/// A titled card on a detail screen. One that starts `collapsed` shows only its title, with its
/// count, until a tap on the title opens it.
struct DetailSection<Content: View>: View {
    let title: String
    let symbol: String
    var collapsed = false
    var count: Int?
    @ViewBuilder let content: Content
    @State private var opened = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if collapsed {
                Button {
                    withAnimation(.smooth) { opened.toggle() }
                } label: {
                    HStack(spacing: 6) {
                        heading
                        Image(systemName: "chevron.right")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(.secondary)
                            .rotationEffect(.degrees(opened ? 90 : 0))
                            .accessibilityHidden(true)
                        Spacer(minLength: 0)
                    }
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(.isHeader)
                .accessibilityValue(opened ? Text("Expanded") : Text("Collapsed"))
                .accessibilityHint(opened ? Text("Hides it") : Text("Shows it"))
            } else {
                heading
            }
            if !collapsed || opened {
                content
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
            }
        }
    }

    private var heading: some View {
        Label(count.map { "\(title) (\($0))" } ?? title, systemImage: symbol)
            .font(.footnote.weight(.bold))
            .textCase(.uppercase)
            .kerning(0.6)
            .foregroundStyle(.secondary)
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
