import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

public extension View {
    /// With nothing saved to show and the Mac not answering, the workspace gives way to the
    /// connection screen. A saved snapshot stays visible and the headers name the problem instead.
    func connectionLostOverlay(_ connection: ConnectionState, hasSavedData: Bool, retry: @escaping () -> Void,
                               signIn: (() -> Void)?, settings: (() -> Void)?,
                               pairAgain: (() -> Void)?, localNetworkSettings: (() -> Void)?) -> some View {
        overlay {
            if case .lost(let problem) = ConnectionIndicators(connection, hasSavedData: hasSavedData).cover {
                ConnectionLostView(problem: problem, retry: retry, signIn: signIn, settings: settings,
                                   pairAgain: pairAgain, localNetworkSettings: localNetworkSettings)
                    .background(DawnBackground())
                    .transition(.opacity)
            }
        }
    }
}
