import AlondraAccount
import AlondraFeatures
import ComposableArchitecture
import AppKit
import SwiftUI

@main
struct AlondraMacApp: App {
    @State private var owner: MacWorkspaceOwner

    init() {
        let auth = AuthSession(browser: SystemAuthBrowser(anchor: {
            NSApplication.shared.keyWindow ?? NSApplication.shared.mainWindow
        }))
        let storage = AppStorageLocation.applicationSupport(bundleID: "com.bitomule.alondra.mac")
        let account = AccountSession(auth: auth, displayName: "Mac", storageDirectory: storage)
        _owner = State(initialValue: MacWorkspaceOwner(account: account, storageDirectory: storage,
            deviceName: "Mac (\(Host.current().localizedName ?? ProcessInfo.processInfo.hostName))"))
    }

    var body: some Scene {
        WindowGroup("Alondra", id: "main") {
            MacServiceRoot(owner: owner)
                .frame(minWidth: 860, minHeight: 540)
        }
        .defaultSize(width: 1240, height: 800)
        .commands {
            CommandGroup(replacing: .newItem) {}
            WorkspaceSettingsCommands()
        }
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
