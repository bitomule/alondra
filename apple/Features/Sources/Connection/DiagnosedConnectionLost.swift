import AlondraContract
import SwiftUI

/// The connection screen for a first load that failed: it asks the connection why, so a Mac in
/// sleep mode reads as a Mac that is not answering rather than as something unexpected.
public struct DiagnosedConnectionLost: View {
    let failure: String
    let diagnose: @Sendable () async -> ConnectionProblem
    let retry: () -> Void
    let signIn: (() -> Void)?
    let settings: (() -> Void)?
    let pairAgain: (() -> Void)?
    @State private var problem: ConnectionProblem = .unexpected

    public init(failure: String, diagnose: @escaping @Sendable () async -> ConnectionProblem,
                retry: @escaping () -> Void, signIn: (() -> Void)?, settings: (() -> Void)?,
                pairAgain: (() -> Void)? = nil) {
        self.failure = failure
        self.diagnose = diagnose
        self.retry = retry
        self.signIn = signIn
        self.settings = settings
        self.pairAgain = pairAgain
    }

    public var body: some View {
        ConnectionLostView(problem: problem, retry: retry, signIn: signIn, settings: settings, pairAgain: pairAgain)
            .task(id: failure) { problem = await diagnose() }
    }
}
