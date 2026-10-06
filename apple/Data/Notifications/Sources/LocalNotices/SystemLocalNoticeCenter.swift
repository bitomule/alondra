import Foundation
import UserNotifications

/// `LocalNoticeCenter` over `UNUserNotificationCenter`. A notification's identifier is its card,
/// and `userInfo.alondra.card` names it for the tap, the way a phone's push does.
public struct SystemLocalNoticeCenter: LocalNoticeCenter {
    public init() {}

    public func requestAuthorization() async {
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
    }

    public func show(_ notice: LocalNotices.Notice) async {
        let content = UNMutableNotificationContent()
        content.title = notice.title
        if let body = notice.body { content.body = body }
        if let thread = notice.thread { content.threadIdentifier = thread }
        content.userInfo = ["alondra": ["card": notice.card]]
        if notice.level == "passive" {
            content.interruptionLevel = .passive
        } else {
            content.interruptionLevel = .active
            content.sound = .default
        }
        let request = UNNotificationRequest(identifier: notice.card, content: content, trigger: nil)
        try? await UNUserNotificationCenter.current().add(request)
    }

    public func delivered() async -> [String] {
        await UNUserNotificationCenter.current().deliveredNotifications().map(\.request.identifier)
    }

    public func remove(_ cards: [String]) async {
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: cards)
    }

    public func setBadge(_ count: Int) async {
        try? await UNUserNotificationCenter.current().setBadgeCount(count)
    }

    public static func card(of response: UNNotificationResponse) -> String? {
        (response.notification.request.content.userInfo["alondra"] as? [String: Any])?["card"] as? String
    }
}
