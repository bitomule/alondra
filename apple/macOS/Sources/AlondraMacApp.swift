import AlondraAppComposition
import AlondraAuth
import AlondraCommands
import AlondraWorkspaceFeature
import ComposableArchitecture
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
                .frame(minWidth: 860, minHeight: 540)
        }
        .defaultSize(width: 1240, height: 800)
        .commands {
            CommandGroup(replacing: .newItem) {}
            WorkspaceSettingsCommands()
        }
    }
}

private struct WorkspaceSettingsStoreKey: FocusedValueKey {
    typealias Value = StoreOf<ServiceRootFeature>
}

extension FocusedValues {
    var workspaceSettingsStore: StoreOf<ServiceRootFeature>? {
        get { self[WorkspaceSettingsStoreKey.self] }
        set { self[WorkspaceSettingsStoreKey.self] = newValue }
    }
}

private struct WorkspaceSettingsCommands: Commands {
    @FocusedValue(\.workspaceSettingsStore) private var store

    var body: some Commands {
        CommandGroup(replacing: .appSettings) {
            Button("Settings…") { store?.send(.settingsPresented(true)) }
                .keyboardShortcut(",")
                .disabled(store == nil)
        }
    }
}
