import AlondraContracts
import SwiftUI

/// The one screen both apps show while the fleet is out of reach. It says what is wrong in
/// plain words and keeps retrying by itself; signing out lives only in Settings.
public struct ConnectionLostView: View {
    let problem: ConnectionProblem?
    let retry: () -> Void
    let signIn: (() -> Void)?
    let settings: (() -> Void)?

    /// `problem == nil` means the first attempt is still in flight.
    public init(problem: ConnectionProblem?, retry: @escaping () -> Void,
                signIn: (() -> Void)? = nil, settings: (() -> Void)? = nil) {
        self.problem = problem
        self.retry = retry
        self.signIn = signIn
        self.settings = settings
    }

    public var body: some View {
        Group {
            if let problem {
                ContentUnavailableView {
                    Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }
                } description: {
                    Text(problem.detail)
                } actions: {
                    actions(for: problem)
                }
            } else {
                VStack(spacing: 16) {
                    ProgressView().controlSize(.large)
                    Text("Connecting to the fleet…").font(.subheadline).foregroundStyle(.secondary)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(.smooth, value: problem)
        .accessibilityIdentifier("connection.lost")
    }

    @ViewBuilder
    private func actions(for problem: ConnectionProblem) -> some View {
        VStack(spacing: 12) {
            if problem == .sessionExpired, let signIn {
                Button("Sign In Again", action: signIn).buttonStyle(.glassProminent)
            } else if problem == .deviceRevoked, let settings {
                Button("Open Settings", action: settings).buttonStyle(.glassProminent)
            } else {
                Button("Try Again", action: retry).buttonStyle(.glassProminent)
            }
            if !problem.needsAction {
                Label { Text("Trying again automatically…") } icon: { ProgressView().controlSize(.small) }
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            if let settings, problem != .deviceRevoked {
                Button("Settings", systemImage: "gearshape", action: settings)
                    .buttonStyle(.borderless)
                    .font(.footnote)
            }
        }
    }
}
