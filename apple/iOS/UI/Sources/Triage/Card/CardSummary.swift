import AlondraContract
import AlondraDesign
import SwiftUI

/// What the card needs of you, why, and who sent it; tapping it opens the detail.
struct CardSummary: View {
    let card: Card
    let why: Bool
    let meta: Bool
    /// Whether the person reads at an accessibility size, before the card caps it.
    let large: Bool
    let open: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(MarkdownBlock.line(card.need ?? card.title)).font(.display(.title2, weight: .bold)).foregroundStyle(Palette.ink)
                .lineLimit(large ? 2 : 3)
                .accessibilityIdentifier("triage.need.\(card.id)")
            if why, let why = card.why {
                Text(MarkdownBlock.line(why)).font(.subheadline).foregroundStyle(.secondary).lineLimit(large ? 2 : 4)
            }
            if meta, let meta = card.meta {
                Text(meta).font(.footnote.weight(.medium)).foregroundStyle(.secondary).lineLimit(large ? 1 : 2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(.rect)
        .onTapGesture(perform: open)
    }
}
