import AlondraNotifications
import Foundation
import Darwin
import os
import UserNotifications

final class NotificationService: UNNotificationServiceExtension {
    private var delivery: Delivery?

    override func didReceive(_ request: UNNotificationRequest, withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        guard let content = request.content.mutableCopy() as? UNMutableNotificationContent else {
            contentHandler(request.content)
            return
        }
        let delivery = Delivery(content: content, handler: contentHandler)
        self.delivery = delivery
        // A service push is opaque until the Mac signature, recipient, expiry and
        // current binding have all been verified. No plaintext push may create actions.
        guard let group = Bundle.main.object(forInfoDictionaryKey: "AlondraPushKeychainAccessGroup") as? String,
              let raw = try? JSONSerialization.data(withJSONObject: content.userInfo),
              let hint = try? NotificationDecoder.bindingHint(raw),
              let context = try? NotificationDecryptionStore(accessGroup: group).load(binding: hint),
              let decrypted = try? NotificationDecoder.decrypt(raw, context: context),
              var payload = (try? JSONSerialization.jsonObject(with: decrypted)) as? [String: Any],
              let binding = NotificationPayload.Binding(userInfo: payload),
              let aps = payload["aps"] as? [String: Any] else {
            delivery.update { content in
                content.categoryIdentifier = ""
                content.title = "Alondra"
                content.body = String(localized: "Open Alondra to view activity")
                content.userInfo = [:]
            }
            delivery.finish(category: nil)
            return
        }
        // Keep the authenticated envelope so the app re-verifies on taps and actions,
        // including when its approved pairing changed since the extension ran.
        if var item = payload["alondra"] as? [String: Any],
           let outer = content.userInfo["alondra"] as? [String: Any] {
            item["envelope"] = outer["envelope"]
            payload["alondra"] = item
        }
        delivery.update { content in
            let alert = aps["alert"] as? [String: Any]
            content.title = alert?["title"] as? String ?? "Alondra"
            content.body = String(localized: "New activity in Alondra")
            content.categoryIdentifier = aps["category"] as? String ?? ""
            content.badge = aps["badge"] as? NSNumber
            content.sound = aps["sound"] as? String == "default" ? .default : nil
            content.userInfo = payload
        }
        let item = NotificationPayload.Item(userInfo: payload)
        Task {
            let category = await QuestionCategories.register(item, binding: binding)
            delivery.finish(category: category)
        }
    }

    override func serviceExtensionTimeWillExpire() {
        delivery?.finish(category: nil)
    }
}

/// Invariant: the content handler runs exactly once, whichever of the category
/// registration or the expiry warning gets there first.
private final class Delivery: Sendable {
    private struct State: @unchecked Sendable {
        var content: UNMutableNotificationContent
        var handler: ((UNNotificationContent) -> Void)?
    }

    private let state: OSAllocatedUnfairLock<State>

    init(content: UNMutableNotificationContent, handler: @escaping (UNNotificationContent) -> Void) {
        state = OSAllocatedUnfairLock(uncheckedState: State(content: content, handler: handler))
    }

    func update(_ work: (UNMutableNotificationContent) -> Void) {
        state.withLockUnchecked { state in
            guard state.handler != nil else { return }
            work(state.content)
        }
    }

    func finish(category: String?) {
        let pending: State? = state.withLockUnchecked { state in
            guard state.handler != nil else { return nil }
            if let category { state.content.categoryIdentifier = category }
            let pending = state
            state.handler = nil
            return pending
        }
        guard let pending, let handler = pending.handler else { return }
        handler(pending.content)
    }
}

enum QuestionCategories {
    static let limit = 20

    private static let registrationPolls = 20
    private static let pollInterval: Duration = .milliseconds(50)

    /// `setNotificationCategories` registers asynchronously; handing the content back before
    /// the new category is visible shows the notification without its buttons.
    static func register(_ item: NotificationPayload.Item?, binding: NotificationPayload.Binding) async -> String? {
        // Notification service instances can run in separate processes. Their common
        // extension container gives flock a lock shared by those processes.
        guard let directory = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else { return nil }
        do { try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true) }
        catch { return nil }
        let fd = open(directory.appending(path: "notification-categories.lock").path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        guard flock(fd, LOCK_EX) == 0 else { return nil }
        defer { flock(fd, LOCK_UN) }
        let center = UNUserNotificationCenter.current()
        let actionableItem = item.flatMap { $0.actions.isEmpty ? nil : $0 }
        let identifier = actionableItem.map { NotificationPayload.Category.questionID($0.card, binding: binding) }
        let existing = await center.notificationCategories()
        let orderURL = directory.appending(path: "notification-category-order.json")
        let previousOrder = (try? Data(contentsOf: orderURL))
            .flatMap { try? JSONDecoder().decode([String].self, from: $0) } ?? []
        let retention = NotificationPayload.Category.retainedCategoryIDs(
            from: existing.map(\.identifier), previousOrder: previousOrder,
            incoming: identifier, limit: limit
        )
        let retained = existing.filter {
            retention.identifiers.contains($0.identifier) && $0.identifier != identifier
        }
        var categories = Set(retained)
        if let actionableItem, let identifier { categories.insert(category(for: actionableItem, identifier: identifier)) }
        do {
            try JSONEncoder().encode(retention.dynamicOrder).write(to: orderURL, options: .atomic)
        } catch {
            return nil
        }
        center.setNotificationCategories(categories)
        for _ in 0..<registrationPolls {
            let visible = await center.notificationCategories()
            if identifier == nil || visible.contains(where: { $0.identifier == identifier }) { return identifier }
            try? await Task.sleep(for: pollInterval)
        }
        return nil
    }

    private static func category(for item: NotificationPayload.Item, identifier: String) -> UNNotificationCategory {
        let actions = item.actions.enumerated().map { index, action -> UNNotificationAction in
            if action.text {
                return UNTextInputNotificationAction(
                    identifier: NotificationPayload.Action.indexed(index), title: action.label,
                    options: [.authenticationRequired], textInputButtonTitle: String(localized: "Send"),
                    textInputPlaceholder: action.placeholder ?? action.label
                )
            }
            return UNNotificationAction(identifier: NotificationPayload.Action.indexed(index), title: action.label,
                                        options: [.authenticationRequired])
        }
        return UNNotificationCategory(identifier: identifier, actions: actions, intentIdentifiers: [])
    }
}
