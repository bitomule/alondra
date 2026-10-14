import AlondraDesign
import SwiftUI

/// What covers the card the moment its answer is sent.
struct CardStamp: View {
    let stamp: String
    let tint: Color

    var body: some View {
        ZStack {
            RadialGradient(colors: [tint.opacity(0.35), tint.opacity(0.05)], center: .center, startRadius: 10, endRadius: 260)
                .clipShape(.rect(cornerRadius: 34, style: .continuous))
            VStack(spacing: 10) {
                Image(systemName: "checkmark").font(.system(size: 44, weight: .bold))
                    .foregroundStyle(Palette.onAccent).frame(width: 92, height: 92)
                    .background(tint, in: .circle).shadow(color: tint.opacity(0.5), radius: 20)
                    .symbolEffect(.bounce, value: stamp)
                Text(stamp).font(.display(.title2, weight: .bold)).foregroundStyle(Palette.ink)
            }
        }
    }
}
