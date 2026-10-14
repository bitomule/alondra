import AlondraContract
import AlondraDesign
import SwiftUI

/// A state or badge of a card, in the colour of its tone.
struct ToneText: View {
    let label: AlondraContract.Label

    var body: some View {
        Text(label.text).font(.caption.weight(.medium)).foregroundStyle(Self.color(label.tone)).lineLimit(2)
    }

    private static func color(_ tone: Tone) -> Color {
        switch tone {
        case .success: Palette.sage
        case .warning: Palette.gold
        case .critical: Palette.rose
        case .accent, .yourTurn: Palette.dawn
        case .info: Palette.sky
        case .neutral: Palette.slate
        }
    }
}
