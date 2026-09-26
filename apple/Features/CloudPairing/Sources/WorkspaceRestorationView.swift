import ComposableArchitecture
import SwiftUI

/// Restoration status lives on the workspace, never in the pairing walkthrough.
public struct WorkspaceRestorationView: View {
    public let store: StoreOf<CloudPairingFeature>
    public init(store: StoreOf<CloudPairingFeature>) { self.store = store }
    public var body: some View {
        VStack(spacing: 8) {
            if let error = store.error {
                Text(error).font(.footnote).multilineTextAlignment(.center)
                if store.reauthenticationRequired {
                    Button("Sign in again") { store.send(.reauthenticate) }
                } else {
                    Button("Retry connection") { store.send(.retryRestore) }
                }
                Button("Sign out", role: .destructive) { store.send(.logout) }
            } else if store.restorationInFlight {
                ProgressView("Connecting to the fleet…").font(.footnote)
            }
        }
        .disabled(store.restorationInFlight)
        .padding()
        .frame(maxWidth: .infinity)
        .accessibilityIdentifier("workspace.restoration")
    }
}
