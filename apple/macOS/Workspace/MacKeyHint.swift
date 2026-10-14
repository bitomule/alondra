import SwiftUI

struct MacKeyHint: View {
    let key: String
    var body: some View {
        Text(key).font(.caption2.monospaced().weight(.semibold))
            .padding(.horizontal, 5).padding(.vertical, 1)
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.foreground.opacity(0.35)))
            .opacity(0.8)
            .accessibilityHidden(true)
    }
}
