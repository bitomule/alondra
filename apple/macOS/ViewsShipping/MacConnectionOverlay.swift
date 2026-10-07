import AlondraFeatures
import AlondraContract
import ComposableArchitecture
import SwiftUI

extension View {
    /// With nothing cached to show, the workspace gives way to the connection screen. A saved
    /// snapshot stays visible and the window subtitle names the problem instead.
    func connectionLostOverlay(store: StoreOf<MacAppFeature>, diagnose: @escaping @Sendable () async -> ConnectionProblem,
                               retry: @escaping () -> Void, signIn: (() -> Void)?,
                               pairAgain: (() -> Void)?) -> some View {
        overlay {
            let hasSnapshot = store.macPresentation.sidebarDocument != nil
            if let failure = store.macPresentation.viewError, !hasSnapshot {
                DiagnosedConnectionLost(failure: failure, diagnose: diagnose, retry: retry, signIn: signIn,
                                        settings: { store.send(.settingsPresented(true)) }, pairAgain: pairAgain)
                    .background(.background)
            }
        }
    }
}
