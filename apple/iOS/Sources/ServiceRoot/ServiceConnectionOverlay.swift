import AlondraConnectionFeature
import AlondraContracts
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

extension View {
    /// With nothing cached to show, the workspace gives way to the connection screen. A saved
    /// snapshot stays visible and the headers name the problem instead.
    func connectionLostOverlay(store: StoreOf<ServiceRootFeature>, retry: @escaping () -> Void,
                               signIn: (() -> Void)?, usesViews: Bool = false) -> some View {
        overlay {
            let problem = usesViews
                ? (store.home.document == nil && store.home.error != nil ? ConnectionProblem.unexpected : nil)
                : store.triage.triage.connection.problem
            let hasSnapshot = usesViews ? store.home.document != nil : store.triage.triage.hasSnapshot
            if let problem, !hasSnapshot {
                ConnectionLostView(problem: problem, retry: retry, signIn: signIn,
                                   settings: { store.send(.settingsPresented(true)) })
                    .background(DawnBackground())
                    .transition(.opacity)
            }
        }
    }
}
