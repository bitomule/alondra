import AlondraConnectionFeature
import AlondraContracts
import AlondraViewsWorkspaceFeature
import ComposableArchitecture
import SwiftUI

extension View {
    /// With nothing cached to show, the workspace gives way to the connection screen. A saved
    /// snapshot stays visible and the window subtitle names the problem instead.
    func connectionLostOverlay(store: StoreOf<ViewsServiceRootFeature>, retry: @escaping () -> Void,
                               signIn: (() -> Void)?) -> some View {
        overlay {
            let problem: ConnectionProblem? = store.macPresentation.sidebarDocument == nil && store.macPresentation.viewError != nil ? .unexpected : nil
            let hasSnapshot = store.macPresentation.sidebarDocument != nil
            if let problem, !hasSnapshot {
                ConnectionLostView(problem: problem, retry: retry, signIn: signIn,
                                   settings: { store.send(.settingsPresented(true)) })
                    .background(.background)
            }
        }
    }
}
