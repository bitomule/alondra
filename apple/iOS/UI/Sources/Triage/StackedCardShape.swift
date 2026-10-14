import AlondraDesign
import SwiftUI

/// A card behind the front one in Waiting for You while nothing moves. Only its top edge shows
/// under the front card, so it is drawn as the card's shell alone: the surface tinted by the card's
/// kind, the hairline and the shadow of a card at rest. It holds no content, takes no touches and
/// is not read aloud.
struct StackedCardShape: View {
    let tint: Color?

    var body: some View {
        ZStack(alignment: .top) {
            Palette.card
            if let tint {
                LinearGradient(colors: [tint.opacity(0.16), tint.opacity(0)], startPoint: .top, endPoint: .bottom)
                    .frame(height: 180)
            }
        }
        .clipShape(.rect(cornerRadius: 34, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 34, style: .continuous).strokeBorder(Palette.hairline) }
        .shadow(color: .black.opacity(0.08), radius: 8, y: 4)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}
