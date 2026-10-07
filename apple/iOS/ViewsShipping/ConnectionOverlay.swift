import AlondraDesign
import AlondraFeatures
import AlondraContract
import ComposableArchitecture
import SwiftUI

extension View {
    /// With nothing cached to show, the workspace gives way to the connection screen. A saved
    /// snapshot stays visible and the headers name the problem instead.
    func connectionLostOverlay(store: StoreOf<AppFeature>, diagnose: @escaping @Sendable () async -> ConnectionProblem,
                               retry: @escaping () -> Void, signIn: (() -> Void)?) -> some View {
        overlay {
            let hasSnapshot = store.triage.screen.document != nil
            if let failure = store.triage.loadError, !hasSnapshot {
                DiagnosedConnectionLost(failure: failure, diagnose: diagnose, retry: retry, signIn: signIn,
                                        settings: { store.send(.settingsPresented(true)) })
                    .background(DawnBackground())
                    .transition(.opacity)
            }
        }
    }
}
