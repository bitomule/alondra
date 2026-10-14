import AlondraDesign
import SwiftUI

/// The card's background: the card colour with its kind's glow along the top.
struct CardSurface: View {
    let tint: Color

    var body: some View {
        ZStack(alignment: .top) {
            Palette.card
            LinearGradient(colors: [tint.opacity(0.16), tint.opacity(0)], startPoint: .top, endPoint: .bottom)
                .frame(height: 180)
        }
    }
}
