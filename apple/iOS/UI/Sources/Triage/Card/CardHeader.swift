import AlondraContract
import AlondraDesign
import SwiftUI

/// The top of a card: its kind as the server names it, its state, how long it has waited and
/// «Leave for Later». When one line is too narrow, the state goes under the kind.
struct CardHeader: View {
    let card: Card
    let showsState: Bool
    let onSkip: (() -> Void)?

    private var state: AlondraContract.Label? { showsState ? card.state : nil }

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) {
                HStack(spacing: 8) {
                    badge
                    if let state { ToneText(label: state) }
                }
                Spacer(minLength: 4)
                trailing
            }
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) { badge; Spacer(minLength: 4); trailing }
                if let state { ToneText(label: state) }
            }
        }
    }

    private var badge: KindBadge { KindBadge(kind: Kind(card), title: card.eyebrow?.text) }

    private var trailing: some View {
        HStack(spacing: 8) {
            if let at = card.timestamps.first?.at {
                Text(relativeSince(at) ?? at).font(.footnote).foregroundStyle(.secondary).lineLimit(1).fixedSize()
            }
            if let onSkip {
                Button(action: onSkip) {
                    Image(systemName: "arrow.uturn.forward")
                        .font(.footnote.weight(.semibold))
                        .frame(width: 30, height: 30)
                        .background(Palette.hairline, in: .circle)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Leave for Later")
                .accessibilityIdentifier("triage.later.\(card.id)")
            }
        }
    }
}
