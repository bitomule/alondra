import AlondraAccount
import AlondraNotifications
import AppKit
import UserNotifications

/// Owns what outlives the window: the workspace route and the system notifications, which keep
/// coming while the app runs with its window closed.
@MainActor
final class MacAppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    let owner: MacWorkspaceOwner
    let notices = MacLocalNotices()

    override init() {
        let auth = AuthSession(browser: SystemAuthBrowser(anchor: {
            NSApplication.shared.keyWindow ?? NSApplication.shared.mainWindow
        }))
        let storage = AppStorageLocation.applicationSupport(bundleID: "com.bitomule.alondra.mac")
        let account = AccountSession(auth: auth, displayName: "Mac", storageDirectory: storage)
        owner = MacWorkspaceOwner(account: account, storageDirectory: storage,
            deviceName: "Mac (\(Host.current().localizedName ?? ProcessInfo.processInfo.hostName))")
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
        Task { [owner, notices] in await notices.run(owner: owner) }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        []
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            didReceive response: UNNotificationResponse) async {
        guard let card = SystemLocalNoticeCenter.card(of: response) else { return }
        await notices.open(card)
    }
}
