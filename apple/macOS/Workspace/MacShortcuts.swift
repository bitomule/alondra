import AlondraContract
import SwiftUI

/// Single-key shortcuts for the buttons of a card or action bar, kept off screen.
struct MacShortcuts: View {
    let buttons: MacCardButtons
    @Binding var moreOpen: Bool
    var onAction: (AlondraContract.Action) -> Void
    var reply: (() -> Void)?

    var body: some View {
        ZStack {
            ForEach(Array((buttons.visible + buttons.more).enumerated()), id: \.offset) { _, action in
                if action.enabled, let key = action.key, key.count == 1, let character = key.first, !(key == "m" && !buttons.more.isEmpty), !(key == "r" && reply != nil) {
                    Button(action.label) { onAction(action) }
                        .keyboardShortcut(KeyEquivalent(character), modifiers: [])
                }
            }
            if !buttons.more.isEmpty {
                Button("More…") { moreOpen.toggle() }.keyboardShortcut("m", modifiers: [])
            }
            if let reply {
                Button("Reply", action: reply).keyboardShortcut("r", modifiers: [])
            }
        }
        .opacity(0).frame(width: 0, height: 0).accessibilityHidden(true)
    }
}
