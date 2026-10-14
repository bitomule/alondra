import AlondraContract
import SwiftUI

/// What a Mac card holds back and what holds it back, in a row while it fits, else in a column.
struct MacBlockChips: View {
    let card: Card
    let open: (AlondraContract.Link) -> Void

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 6) { chips }
            VStack(alignment: .leading, spacing: 6) { chips }
        }
    }

    @ViewBuilder private var chips: some View {
        ForEach(card.blocks, id: \.id) { chip($0, holdsBack: false) }
        ForEach(card.blockedBy, id: \.id) { chip($0, holdsBack: true) }
    }

    private func chip(_ chip: BlockChip, holdsBack: Bool) -> some View {
        let tint = holdsBack ? MacCardPalette.tone(.warning) : Color.secondary
        return Button { if let link = chip.link { open(link) } } label: {
            Label(chip.label, systemImage: holdsBack ? "lock" : "arrow.turn.down.right")
                .font(.caption.weight(.medium)).lineLimit(1).foregroundStyle(tint)
                .padding(.horizontal, 9).padding(.vertical, 4)
                .background(tint.opacity(0.12), in: .capsule)
        }
        .buttonStyle(.plain)
        .disabled(chip.link == nil)
    }
}
