import AlondraAppComposition
import AlondraAuth
import AlondraCommands
import AppKit
import SwiftUI

@main
struct AlondraMacApp: App {
    @State private var account: CloudPairingOwner

    init() {
        let auth = AuthSession(browser: SystemAuthBrowser(anchor: {
            NSApplication.shared.keyWindow ?? NSApplication.shared.mainWindow
        }))
        _account = State(initialValue: CloudPairingOwner(auth: auth, displayName: "Mac",
            storageDirectory: CommandJournalLocation.applicationSupport(bundleID: "com.bitomule.alondra.mac")))
    }

    var body: some Scene {
        WindowGroup("Alondra", id: "main") {
            MacServiceRoot(account: account)
                .frame(minWidth: 720, minHeight: 500)
        }
        .defaultSize(width: 1100, height: 760)
        .commands {
            CommandGroup(replacing: .newItem) {}
        }
    }
}
