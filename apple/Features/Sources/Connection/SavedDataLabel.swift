import AlondraContract
import SwiftUI

/// What a screen showing saved data needs to say why it is not current and offer the way out.
public struct ConnectionRecovery {
    public let diagnose: @Sendable () async -> ConnectionProblem
    public let retry: () -> Void
    public let localNetworkSettings: (() -> Void)?
    /// What is already known to be wrong, shown until a fresh diagnosis comes back.
    public let known: ConnectionProblem?

    public init(diagnose: @escaping @Sendable () async -> ConnectionProblem, retry: @escaping () -> Void,
                localNetworkSettings: (() -> Void)?, known: ConnectionProblem? = nil) {
        self.diagnose = diagnose
        self.retry = retry
        self.localNetworkSettings = localNetworkSettings
        self.known = known
    }
}

public extension EnvironmentValues {
    @Entry var connectionRecovery: ConnectionRecovery?
}

/// «saved data · <why>» over a snapshot that could not be refreshed. Tapping it shows the whole
/// problem and what to do; the screen keeps retrying on its own meanwhile.
public struct SavedDataLabel: View {
    @Environment(\.connectionRecovery) private var recovery
    @State private var diagnosed: ConnectionProblem?
    @State private var showsProblem = false

    public init() {}

    private var problem: ConnectionProblem? { diagnosed ?? recovery?.known }

    private var label: some View {
        Label {
            Text("saved data · \(Text((problem ?? .unexpected).summary))")
        } icon: {
            Image(systemName: problem?.systemImage ?? "wifi.slash")
        }
    }

    public var body: some View {
        Group {
            if recovery == nil {
                label
            } else {
                Button { showsProblem = true } label: { label }
                    .buttonStyle(.plain)
            }
        }
        .accessibilityIdentifier("connection.savedData")
        .task { diagnosed = await recovery?.diagnose() }
        .sheet(isPresented: $showsProblem) {
            if let recovery {
                ConnectionLostView(problem: problem, retry: {
                    showsProblem = false
                    recovery.retry()
                }, localNetworkSettings: recovery.localNetworkSettings)
                .presentationDetents([.medium])
            }
        }
    }
}
