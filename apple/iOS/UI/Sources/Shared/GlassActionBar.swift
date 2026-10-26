import AlondraDesign
import SwiftUI

/// The buttons floating at the foot of a review screen, as one Liquid Glass group: every button
/// one line and one height, the main action prominent and tinted, the rest plain glass.
struct GlassActionBar<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        GlassEffectContainer(spacing: 10) {
            VStack(spacing: 10) { content }
                .controlSize(.large)
                .buttonBorderShape(.capsule)
                .font(.headline)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }
}

/// The bar's buttons in the roomiest layout that fits: all on one line; else the main button
/// alone with the rest and the accessory under it; else one per line (large text, long labels).
/// The accessory is told whether it sits in a line (compact) or stacked, to draw itself to match.
struct GlassBarRow<Primary: View, Secondary: View, Accessory: View>: View {
    @ViewBuilder let primary: Primary
    @ViewBuilder let secondary: Secondary
    @ViewBuilder let accessory: (_ stacked: Bool) -> Accessory

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) {
                primary
                secondary
                accessory(false)
            }
            .lineLimit(1)
            VStack(spacing: 10) {
                primary
                HStack(spacing: 10) {
                    secondary
                    accessory(false)
                }
                .lineLimit(1)
            }
            VStack(spacing: 10) {
                primary
                secondary
                accessory(true)
            }
        }
    }
}

/// A bar button's label: it drops its symbol before its words when the row runs short, and
/// stretches so the buttons sharing a row split it evenly.
struct GlassBarLabel: View {
    let title: String
    let systemImage: String?

    init(_ title: String, systemImage: String? = nil) {
        self.title = title
        self.systemImage = systemImage
    }

    var body: some View {
        ViewThatFits(in: .horizontal) {
            if let systemImage { Label { Text(MarkdownBlock.line(title)) } icon: { Image(systemName: systemImage) } }
            Text(MarkdownBlock.line(title))
        }
        .frame(maxWidth: .infinity)
    }
}
