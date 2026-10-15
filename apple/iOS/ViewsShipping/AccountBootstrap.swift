import AlondraDesign
import AlondraAccount
import AlondraIroh
import AVFoundation
import UIKit
import VisionKit

@MainActor
func makeAccountSession(notifications: NotificationRuntime) -> AccountSession {
    let browser = SystemAuthBrowser(anchor: {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first(where: { $0.activationState == .foregroundActive })?.windows.first(where: \.isKeyWindow)
    })
    let auth = AuthSession(browser: browser, refreshGrant: keepRefreshAlive,
                           sessionLost: { Task { @MainActor in notifications.retireBindings() } })
    let account = AccountSession(auth: auth, displayName: "iPhone",
        storageDirectory: AppStorageLocation.applicationSupport(bundleID: "com.bitomule.alondra"), prepareCamera: {
        guard await DataScannerViewController.isSupported else { return false }
        let allowed = await AVCaptureDevice.requestAccess(for: .video)
        let available = await MainActor.run { DataScannerViewController.isAvailable }
        return allowed && available
    }, didReconcile: { snapshot in
        return try await notifications.prepare(snapshot)
    }, willDeactivate: {
        try await notifications.deactivate()
    }, pushBinding: { notifications.pushBinding(for: $0) }, irohDialer: { IrohEndpointDialer(identity: $0) },
        startsLive: UIApplication.shared.applicationState != .background)
    account.isActive = { UIApplication.shared.applicationState == .active }
    account.openLocalNetworkSettings = {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }
    return account
}

/// A refresh cut off by suspension loses the issuer's answer; finish it in the background.
private let keepRefreshAlive = pairedRefreshGrant(begin: { expiration in
    let task = UIApplication.shared.beginBackgroundTask(withName: "alondra.auth-refresh", expirationHandler: expiration)
    return task == .invalid ? nil : task
}, end: { UIApplication.shared.endBackgroundTask($0) })
