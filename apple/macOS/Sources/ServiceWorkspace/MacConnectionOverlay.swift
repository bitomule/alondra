import AlondraConnectionFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

extension View {
    /// With nothing cached to show, the workspace gives way to the connection screen. A saved
    /// snapshot stays visible and the window subtitle names the problem instead.
    func connectionLostOverlay(store: StoreOf<ServiceRootFeature>, retry: @escaping () -> Void,
                               signIn: (() -> Void)?) -> some View {
        overlay {
            if let problem = store.triage.triage.connection.problem, !store.triage.triage.hasSnapshot {
                ConnectionLostView(problem: problem, retry: retry, signIn: signIn,
                                   settings: { store.send(.settingsPresented(true)) })
                    .background(.background)
            }
        }
    }
}
