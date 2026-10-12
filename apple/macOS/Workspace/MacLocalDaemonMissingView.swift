import AlondraFeatures
import AlondraContract
import SwiftUI

/// Shown when neither this Mac's daemon nor a remote Mac is available. The window keeps
/// looking for the local daemon and connects on its own as soon as it answers.
public struct MacLocalDaemonMissingView: View {
    let connectToAnotherMac: () -> Void
    let exploreDemo: (() -> Void)?

    public init(connectToAnotherMac: @escaping () -> Void, exploreDemo: (() -> Void)? = nil) {
        self.connectToAnotherMac = connectToAnotherMac
        self.exploreDemo = exploreDemo
    }

    public var body: some View {
        ContentUnavailableView {
            Label { Text(ConnectionProblem.alondraNotRunning.title) } icon: {
                Image(systemName: ConnectionProblem.alondraNotRunning.systemImage)
            }
        } description: {
            VStack(alignment: .leading, spacing: 10) {
                Text("Install it with Homebrew, then run it once:")
                InstallCommands()
                Text("This window connects as soon as it answers.").foregroundStyle(.secondary)
            }
            .frame(maxWidth: 420)
        } actions: {
            Button("Connect to Another Mac…", action: connectToAnotherMac)
            if let exploreDemo { Button("Explore the demo", action: exploreDemo).accessibilityIdentifier("demo.enter") }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("connection.localDaemonMissing")
    }
}
