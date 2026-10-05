import Foundation

/// Which notifications on the phone stand for cards that no longer wait for the user. A card
/// answered, withdrawn or closed anywhere leaves the queue; its notification must leave too.
public enum DeliveredNotificationSync {
    /// What a Mac says waits for the user: its whole queue, or only the cards that left it
    /// when the whole queue is too long for a push.
    public enum Queue: Equatable, Sendable {
        case open(Set<String>)
        case retracted(Set<String>)

        func waits(_ card: String) -> Bool {
            switch self {
            case .open(let cards): cards.contains(card)
            case .retracted(let cards): !cards.contains(card)
            }
        }
    }

    /// A notification in Notification Center: the card and Mac it was pushed for, which the
    /// notification service extension keeps in `userInfo.alondra`.
    public struct Delivered: Equatable, Sendable {
        public let identifier: String
        public let card: String?
        public let binding: NotificationPayload.Binding?

        public init(identifier: String, userInfo: [AnyHashable: Any]) {
            let alondra = userInfo["alondra"] as? [String: Any]
            self.identifier = identifier
            card = alondra?["card"] as? String
            binding = NotificationPayload.Binding(userInfo: userInfo)
        }
    }

    /// The authenticated content of a retraction push: the Mac's queue and its badge.
    public struct Retraction: Equatable, Sendable {
        public let binding: NotificationPayload.Binding
        public let queue: Queue
        public let badge: Int?

        public init?(userInfo: [AnyHashable: Any]) {
            guard let binding = NotificationPayload.Binding(userInfo: userInfo),
                  let alondra = userInfo["alondra"] as? [String: Any],
                  alondra["kind"] as? String == "retract" else { return nil }
            if let open = alondra["open"] as? [String] {
                queue = .open(Set(open))
            } else if let retracted = alondra["retracted"] as? [String] {
                queue = .retracted(Set(retracted))
            } else {
                return nil
            }
            self.binding = binding
            badge = ((userInfo["aps"] as? [String: Any])?["badge"] as? NSNumber)?.intValue
        }
    }

    /// Only this Mac's card notifications are touched: another Mac's queue says nothing about
    /// them, and the app's own notices ("No longer pending", "Couldn't send") carry no card.
    public static func stale(_ delivered: [Delivered], binding: NotificationPayload.Binding, queue: Queue) -> [String] {
        delivered.compactMap { notification in
            guard notification.binding == binding, let card = notification.card, !queue.waits(card) else { return nil }
            return notification.identifier
        }
    }
}
