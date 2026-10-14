import AlondraContract
import SwiftUI

/// An action bar the way a card shows its buttons: three by role, the rest behind "More…".
struct MacActionBar: View {
    let buttons: MacCardButtons
    var onAction: (AlondraContract.Action) -> Void
    @State private var moreOpen = false

    init(bar: ActionBar, onAction: @escaping (AlondraContract.Action) -> Void) {
        buttons = MacCardButtons(bar: bar.actions)
        self.onAction = onAction
    }

    var body: some View {
        MacActionColumn(buttons: buttons, moreOpen: $moreOpen, onAction: onAction)
            .frame(maxWidth: 640)
            .background { MacShortcuts(buttons: buttons, moreOpen: $moreOpen, onAction: onAction, reply: nil) }
    }
}
