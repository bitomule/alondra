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
        let name = Host.current().localizedName ?? ProcessInfo.processInfo.hostName
        let account = AccountSession(auth: auth, displayName: String(name.prefix(80)), storageDirectory: storage)
        account.openLocalNetworkSettings = {
            guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork") else { return }
            NSWorkspace.shared.open(url)
        }
        owner = MacWorkspaceOwner(account: account, storageDirectory: storage, deviceName: "Mac (\(name))")
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
        Task { [owner, notices] in await notices.run(owner: owner) }
    }

    /// With the app in front, the window shows the same notice; a pairing request as a sheet.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        []
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            didReceive response: UNNotificationResponse) async {
        if let pairing = MacLocalNotices.pairing(of: response) {
            switch response.actionIdentifier {
            case MacLocalNotices.allowAction: await notices.decide(pairing, allow: true)
            case MacLocalNotices.denyAction: await notices.decide(pairing, allow: false)
            default: await notices.bringForward()
            }
            return
        }
        guard let card = SystemLocalNoticeCenter.card(of: response) else { return }
        await notices.open(card)
    }
}
