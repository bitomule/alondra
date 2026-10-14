import AlondraContract
import AlondraFeatures
import SwiftUI

public extension View {
    /// With nothing saved to show and the Mac not answering, the window gives way to the
    /// connection screen. A saved sidebar stays visible and the subtitle names the problem instead.
    func connectionLostOverlay(_ connection: ConnectionState, hasSavedData: Bool, retry: @escaping () -> Void,
                               signIn: (() -> Void)?, settings: (() -> Void)?,
                               pairAgain: (() -> Void)?, localNetworkSettings: (() -> Void)?) -> some View {
        overlay {
            if case .lost(let problem) = ConnectionIndicators(connection, hasSavedData: hasSavedData).cover {
                ConnectionLostView(problem: problem, retry: retry, signIn: signIn, settings: settings,
                                   pairAgain: pairAgain, localNetworkSettings: localNetworkSettings)
                    .background(.background)
            }
        }
    }
}
