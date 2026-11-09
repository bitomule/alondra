import AlondraAccount
import AlondraDesign
import AlondraNetworkReachability
import SwiftUI
import UIKit

@main
struct AlondraApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        WindowGroup {
            AppScene(owner: delegate.cloudPairing, notifications: delegate.notifications, liveActivity: delegate.liveActivity)
                .environment(delegate.cloudPairing)
                .tint(Palette.dawn)
        }
    }
}

/// Process ownership is limited to credentials and the approved service session.
/// Each WindowGroup instance creates its own presentation stores.
final class AppDelegate: NSObject, UIApplicationDelegate {
    let notifications = NotificationRuntime()
    let liveActivity = LiveActivityRuntime()
    lazy var cloudPairing = makeAccountSession(notifications: notifications)

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        liveActivity.onRegistrationChange = { [weak notifications] failures in
            notifications?.activityRegistrationFailures = failures
        }
        liveActivity.start()
        notifications.pairingsChanged = { [liveActivity] clients, contexts in liveActivity.relayPairings(clients, contexts) }
        let owner = cloudPairing
        notifications.prepareSession = { [weak owner] in try await owner?.prepareNotificationSession() }
        notifications.selectSession = { [weak owner] id in
            guard let owner else { throw CancellationError() }
            try await owner.selectMac(id)
        }
        notifications.installDelegate()
        notifications.drainUnregistrations()
        return true
    }
    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        notifications.push.didRegister(deviceToken: deviceToken)
    }
    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        notifications.push.didFailToRegister(error)
    }
    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
        NetworkReachability.shared.hint()
        return await notifications.retract(userInfo) ? .newData : .noData
    }
}
