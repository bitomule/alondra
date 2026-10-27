import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// The documents a card carries, one row each, opening the document on its own screen.
struct MacDocumentLinks: View {
    let facts: Facts
    let onLink: (AlondraContract.Link) -> Void

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(facts.items.enumerated()), id: \.offset) { index, fact in
                if index > 0 { Divider().padding(.leading, 40) }
                Button { if let link = fact.link { onLink(link) } } label: { row(fact) }
                    .buttonStyle(.plain)
                    .disabled(fact.link == nil)
                    .accessibilityIdentifier("mac.document.\(index)")
            }
        }
        .background(Palette.card, in: .rect(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(.separator.opacity(0.6)))
    }

    private func row(_ fact: Fact) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "doc.text").foregroundStyle(.secondary).frame(width: 20)
            VStack(alignment: .leading, spacing: 1) {
                Text(MarkdownBlock.line(fact.value)).font(.body.weight(.medium)).foregroundStyle(Palette.ink)
                Text(fact.label).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .contentShape(Rectangle())
    }
}
