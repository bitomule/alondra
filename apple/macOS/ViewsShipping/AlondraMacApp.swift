import AlondraAccount
import AlondraFeatures
import AlondraMacWorkspace
import ComposableArchitecture
import AppKit
import SwiftUI

@main
struct AlondraMacApp: App {
    @NSApplicationDelegateAdaptor private var delegate: MacAppDelegate

    var body: some Scene {
        WindowGroup("Alondra", id: "main") {
            MacServiceRoot(owner: delegate.owner, notices: delegate.notices)
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
