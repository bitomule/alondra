import AlondraAppComposition
import AlondraAuth
import AlondraCommands
import AVFoundation
import UIKit
import VisionKit

@MainActor
func makeCloudPairingOwner(notifications: ServiceNotificationRuntime) -> CloudPairingOwner {
    let browser = SystemAuthBrowser(anchor: {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first(where: { $0.activationState == .foregroundActive })?.windows.first(where: \.isKeyWindow)
    })
    return CloudPairingOwner(auth: AuthSession(browser: browser), displayName: "iPhone",
        storageDirectory: CommandJournalLocation.applicationSupport(bundleID: "com.bitomule.alondra"), prepareCamera: {
        guard await DataScannerViewController.isSupported else { return false }
        let allowed = await AVCaptureDevice.requestAccess(for: .video)
        let available = await MainActor.run { DataScannerViewController.isAvailable }
        return allowed && available
    }, didActivate: { approved, services in
        try await notifications.activate(approved, services: services)
    }, willDeactivate: { services in
        try await notifications.deactivate(services)
    })
}
