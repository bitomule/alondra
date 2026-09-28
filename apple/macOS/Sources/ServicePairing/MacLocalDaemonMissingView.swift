import SwiftUI

/// Shown when neither this Mac's daemon nor a remote Mac is available. The window keeps
/// looking for the local daemon and connects on its own as soon as it answers.
struct MacLocalDaemonMissingView: View {
    let connectToAnotherMac: () -> Void

    var body: some View {
        ContentUnavailableView {
            Label("Alondra isn’t running on this Mac", systemImage: "bolt.horizontal.circle")
        } description: {
            Text("Run `alondra setup` in Terminal to start it. This window connects as soon as it answers.")
        } actions: {
            Button("Connect to Another Mac…", action: connectToAnotherMac)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("connection.localDaemonMissing")
    }
}
