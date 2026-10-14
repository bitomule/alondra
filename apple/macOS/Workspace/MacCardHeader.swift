import AlondraContract
import SwiftUI

/// The top of a Mac card: its kind as the server names it, in its colour, and its state.
struct MacCardHeader: View {
    let card: Card

    private var tint: Color { MacCardPalette.kind(card.kind) }

    var body: some View {
        HStack(spacing: 8) {
            Circle().fill(tint).frame(width: 8, height: 8)
            if let eyebrow = card.eyebrow {
                Text(eyebrow.text).font(.caption.weight(.bold)).kerning(1).textCase(.uppercase).foregroundStyle(tint)
            }
            Spacer(minLength: 8)
            if let state = card.state {
                Text(state.text).font(.caption).foregroundStyle(MacCardPalette.tone(state.tone)).lineLimit(1)
            }
        }
    }
}
