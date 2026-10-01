import AlondraContract
import ComposableArchitecture
import SwiftUI

public extension SignInFeature.State {
    /// What the connection screen shows while the saved workspace is being restored.
    var connectionProblem: ConnectionProblem? {
        if logoutIncomplete { return .unexpected }
        return restoreProblem
    }
}

/// Shown instead of the workspace until the saved pairing is restored. Retries on its own; the
/// only way to sign out from here is the Settings sheet.
public struct RestoreConnectionView: View {
    let store: StoreOf<SignInFeature>
    let networkChanges: @Sendable () -> AsyncStream<Bool>
    @State private var settingsPresented = false

    public init(store: StoreOf<SignInFeature>, networkChanges: @escaping @Sendable () -> AsyncStream<Bool>) {
        self.store = store
        self.networkChanges = networkChanges
    }

    public var body: some View {
        ConnectionLostView(problem: store.connectionProblem,
                           retry: { store.send(.retryRestore) },
                           signIn: { store.send(.reauthenticate) },
                           settings: { settingsPresented = true })
            .disabled(store.restorationInFlight && store.connectionProblem?.needsAction == true)
            .retriesConnection(networkChanges: networkChanges) { store.send(.retryNow) }
            .sheet(isPresented: $settingsPresented) {
                RestoreSettingsSheet(store: store)
            }
    }
}

private struct RestoreSettingsSheet: View {
    let store: StoreOf<SignInFeature>
    @Environment(\.dismiss) private var dismiss
    @State private var confirmLogout = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Button("Sign out", systemImage: "link.badge.minus", role: .destructive) { confirmLogout = true }
                } header: {
                    Text("Account")
                } footer: {
                    Text("Signing out removes this device’s saved connection and notification keys.")
                }
            }
            .formStyle(.grouped)
            .navigationTitle("Settings")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
            }
            .confirmationDialog("Sign out of Alondra?", isPresented: $confirmLogout, titleVisibility: .visible) {
                Button("Sign out", role: .destructive) {
                    store.send(.logout)
                    dismiss()
                }
                Button("Cancel", role: .cancel) {}
            }
        }
        #if os(macOS)
        .frame(minWidth: 420, minHeight: 240)
        #endif
    }
}
