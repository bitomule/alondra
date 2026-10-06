import AlondraDesign
import AlondraAccount
import AVFoundation
import UIKit
import VisionKit

@MainActor
func makeAccountSession(notifications: NotificationRuntime) -> AccountSession {
    let browser = SystemAuthBrowser(anchor: {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first(where: { $0.activationState == .foregroundActive })?.windows.first(where: \.isKeyWindow)
    })
    return AccountSession(auth: AuthSession(browser: browser, refreshGrant: keepRefreshAlive), displayName: "iPhone",
        storageDirectory: AppStorageLocation.applicationSupport(bundleID: "com.bitomule.alondra"), prepareCamera: {
        guard await DataScannerViewController.isSupported else { return false }
        let allowed = await AVCaptureDevice.requestAccess(for: .video)
        let available = await MainActor.run { DataScannerViewController.isAvailable }
        return allowed && available
    }, didReconcile: { snapshot in
        return try await notifications.prepare(snapshot)
    }, willDeactivate: {
        try await notifications.deactivate()
    })
}

/// A refresh cut off by suspension loses the issuer's answer; finish it in the background.
@Sendable private func keepRefreshAlive() async -> @Sendable () async -> Void {
    let task = await MainActor.run { UIApplication.shared.beginBackgroundTask(withName: "alondra.auth-refresh") }
    return {
        await MainActor.run {
            guard task != .invalid else { return }
            UIApplication.shared.endBackgroundTask(task)
        }
    }
}
