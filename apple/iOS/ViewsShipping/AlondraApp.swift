import AlondraAccount
import AlondraDesign
import SwiftUI
import UIKit

@main
struct AlondraApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        WindowGroup {
            AppScene(owner: delegate.cloudPairing, notifications: delegate.notifications)
                .environment(delegate.cloudPairing)
                .tint(Palette.dawn)
        }
    }
}

/// Process ownership is limited to credentials and the approved service session.
/// Each WindowGroup instance creates its own presentation stores.
final class AppDelegate: NSObject, UIApplicationDelegate {
    let notifications = NotificationRuntime()
    lazy var cloudPairing = makeAccountSession(notifications: notifications)

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        let owner = cloudPairing
        notifications.prepareSession = { [weak owner] in try await owner?.prepareNotificationSession() }
        notifications.selectSession = { [weak owner] id in
            guard let owner else { throw CancellationError() }
            try await owner.selectMac(id)
        }
        notifications.installDelegate()
        return true
    }
    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        notifications.push.didRegister(deviceToken: deviceToken)
    }
    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        notifications.push.didFailToRegister(error)
    }
    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
        await notifications.retract(userInfo) ? .newData : .noData
    }
}
