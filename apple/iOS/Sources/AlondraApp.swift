import AlondraAppComposition
import AlondraShared
import SwiftUI
import UIKit

@main
struct AlondraApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        WindowGroup {
            ServiceAppScene(owner: delegate.cloudPairing)
                .environment(delegate.cloudPairing)
                .tint(Palette.dawn)
        }
    }
}

/// Process ownership is limited to credentials and the approved service session.
/// Each WindowGroup instance creates its own presentation stores.
final class AppDelegate: NSObject, UIApplicationDelegate {
    let cloudPairing = makeCloudPairingOwner()
}
