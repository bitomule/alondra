import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// The documents a card carries, one row each, opening the document on its own screen.
struct DocumentLinks: View {
    let facts: Facts
    let tint: Color
    let open: (AlondraContract.Link) -> Void

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(facts.items.enumerated()), id: \.offset) { index, fact in
                if index > 0 { Divider().padding(.leading, 52) }
                Button { if let link = fact.link { open(link) } } label: { row(fact) }
                    .buttonStyle(.plain)
                    .disabled(fact.link == nil)
                    .accessibilityIdentifier("cardDetail.document.\(index)")
            }
        }
        .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 16, style: .continuous))
    }

    private func row(_ fact: Fact) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "doc.text").font(.body.weight(.semibold)).foregroundStyle(tint).frame(width: 26)
            VStack(alignment: .leading, spacing: 2) {
                Text(MarkdownBlock.line(fact.value)).font(.subheadline.weight(.semibold)).foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.leading)
                Text(fact.label).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
        .contentShape(Rectangle())
    }
}
