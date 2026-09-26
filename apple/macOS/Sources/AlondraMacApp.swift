import AlondraAppComposition
import AlondraAuth
import AppKit
import SwiftUI

@main
struct AlondraMacApp: App {
    @State private var account: CloudPairingOwner

    init() {
        let auth = AuthSession(browser: SystemAuthBrowser(anchor: {
            NSApplication.shared.keyWindow ?? NSApplication.shared.mainWindow ?? NSWindow()
        }))
        _account = State(initialValue: CloudPairingOwner(auth: auth, displayName: "Mac"))
    }

    var body: some Scene {
        WindowGroup("Alondra", id: "main") {
            MacServiceRoot(account: account)
                .frame(minWidth: 720, minHeight: 500)
        }
        .defaultSize(width: 1100, height: 760)
    }
}
