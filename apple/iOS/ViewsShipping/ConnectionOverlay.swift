import AlondraDesign
import AlondraFeatures
import AlondraContract
import ComposableArchitecture
import SwiftUI

extension View {
    /// With nothing cached to show, the workspace gives way to the connection screen. A saved
    /// snapshot stays visible and the headers name the problem instead.
    func connectionLostOverlay(store: StoreOf<AppFeature>, retry: @escaping () -> Void,
                               signIn: (() -> Void)?) -> some View {
        overlay {
            let problem: ConnectionProblem? = store.triage.loadError != nil ? .unexpected : nil
            let hasSnapshot = store.triage.screen.document != nil
            if let problem, !hasSnapshot {
                ConnectionLostView(problem: problem, retry: retry, signIn: signIn,
                                   settings: { store.send(.settingsPresented(true)) })
                    .background(DawnBackground())
                    .transition(.opacity)
            }
        }
    }
}
