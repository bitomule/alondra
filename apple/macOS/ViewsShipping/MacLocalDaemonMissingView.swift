import AlondraFeatures
import AlondraContract
import SwiftUI

/// Shown when neither this Mac's daemon nor a remote Mac is available. The window keeps
/// looking for the local daemon and connects on its own as soon as it answers.
struct MacLocalDaemonMissingView: View {
    let connectToAnotherMac: () -> Void
    var exploreDemo: (() -> Void)? = nil

    var body: some View {
        ContentUnavailableView {
            Label { Text(ConnectionProblem.alondraNotRunning.title) } icon: {
                Image(systemName: ConnectionProblem.alondraNotRunning.systemImage)
            }
        } description: {
            Text(ConnectionProblem.alondraNotRunning.detail)
        } actions: {
            Button("Connect to Another Mac…", action: connectToAnotherMac)
            if let exploreDemo { Button("Explore the demo", action: exploreDemo).accessibilityIdentifier("demo.enter") }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("connection.localDaemonMissing")
    }
}
