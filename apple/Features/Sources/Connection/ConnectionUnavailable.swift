import AlondraContract
import SwiftUI

/// What a screen with nothing saved shows in its place while the Mac is not answering: the same
/// problem the rest of the app shows, with a way to try again when trying again can help.
public struct ConnectionUnavailable: View {
    let problem: ConnectionProblem
    let retry: () -> Void

    public init(problem: ConnectionProblem, retry: @escaping () -> Void) {
        self.problem = problem
        self.retry = retry
    }

    public var body: some View {
        ContentUnavailableView {
            Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }
        } description: {
            Text(problem.detail)
        } actions: {
            if !problem.needsAction {
                Button("Try Again", action: retry)
            }
        }
        .accessibilityIdentifier("connection.unavailable")
    }
}
