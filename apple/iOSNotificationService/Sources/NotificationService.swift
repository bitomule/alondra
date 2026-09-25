import AlondraNotificationPayload
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
        Task {
            let category = await QuestionCategories.register(NotificationPayload.Question(userInfo: content.userInfo))
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

    private static var fixedCategories: Set<UNNotificationCategory> {
        [
            UNNotificationCategory(identifier: NotificationPayload.Category.question, actions: [
                replyAction,
                UNNotificationAction(identifier: NotificationPayload.Action.moreContext, title: String(localized: "I need more context"), icon: UNNotificationActionIcon(systemImageName: "questionmark.bubble")),
            ], intentIdentifiers: []),
            UNNotificationCategory(identifier: NotificationPayload.Category.permission, actions: [
                UNNotificationAction(identifier: NotificationPayload.Action.allow, title: String(localized: "Allow"), options: [.authenticationRequired], icon: UNNotificationActionIcon(systemImageName: "checkmark")),
                UNNotificationAction(identifier: NotificationPayload.Action.deny, title: String(localized: "Deny"), options: [.destructive], icon: UNNotificationActionIcon(systemImageName: "xmark")),
            ], intentIdentifiers: []),
            UNNotificationCategory(identifier: NotificationPayload.Category.plan, actions: [
                UNNotificationAction(identifier: NotificationPayload.Action.approve, title: String(localized: "Approve"), options: [.authenticationRequired], icon: UNNotificationActionIcon(systemImageName: "checkmark")),
                UNNotificationAction(identifier: NotificationPayload.Action.review, title: String(localized: "Review"), options: [.foreground], icon: UNNotificationActionIcon(systemImageName: "doc.text.magnifyingglass")),
            ], intentIdentifiers: []),
            UNNotificationCategory(identifier: NotificationPayload.Category.read, actions: [
                UNNotificationAction(identifier: NotificationPayload.Action.markRead, title: String(localized: "Mark as Read"), icon: UNNotificationActionIcon(systemImageName: "checkmark")),
            ], intentIdentifiers: []),
        ]
    }

    private static var replyAction: UNTextInputNotificationAction {
        UNTextInputNotificationAction(
            identifier: NotificationPayload.Action.reply,
            title: String(localized: "Reply"),
            options: [.authenticationRequired],
            icon: UNNotificationActionIcon(systemImageName: "arrowshape.turn.up.left"),
            textInputButtonTitle: String(localized: "Send"),
            textInputPlaceholder: String(localized: "Your answer")
        )
    }

    private static let registrationPolls = 20
    private static let pollInterval: Duration = .milliseconds(50)

    /// `setNotificationCategories` registers asynchronously; handing the content back before
    /// the new category is visible shows the notification without its buttons.
    static func register(_ question: NotificationPayload.Question?) async -> String? {
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
        let identifier = question.map { NotificationPayload.Category.questionID($0.id) }
        let existing = await center.notificationCategories()
        let recent = existing
            .compactMap { category in NotificationPayload.Category.questionID(from: category.identifier).map { (id: $0, category: category) } }
            .filter { $0.id != question?.id }
            .sorted { $0.id < $1.id }
            .suffix(limit - 1)
            .map(\.category)
        let fixedIDs = Set(fixedCategories.map(\.identifier))
        let other = existing.filter {
            NotificationPayload.Category.questionID(from: $0.identifier) == nil && !fixedIDs.contains($0.identifier)
        }
        var categories = other.union(recent).union(fixedCategories)
        if let question, let identifier { categories.insert(category(for: question, identifier: identifier)) }
        center.setNotificationCategories(categories)
        for _ in 0..<registrationPolls {
            let visible = await center.notificationCategories()
            if fixedCategories.allSatisfy({ fixed in visible.contains(where: { $0.identifier == fixed.identifier }) }) &&
                (identifier == nil || visible.contains(where: { $0.identifier == identifier })) { return identifier }
            try? await Task.sleep(for: pollInterval)
        }
        return nil
    }

    private static func title(for label: String) -> String {
        switch label {
        case "Approve": String(localized: "Approve")
        case "Request changes": String(localized: "Request changes")
        case "Allow": String(localized: "Allow")
        case "Deny": String(localized: "Deny")
        default: label
        }
    }

    private static func category(for question: NotificationPayload.Question, identifier: String) -> UNNotificationCategory {
        let options = question.options.map { label in
            UNNotificationAction(identifier: NotificationPayload.Action.option(label), title: title(for: label), options: [.authenticationRequired])
        }
        return UNNotificationCategory(identifier: identifier, actions: options + [
            replyAction,
            UNNotificationAction(identifier: NotificationPayload.Action.moreContext, title: String(localized: "I need more context"), icon: UNNotificationActionIcon(systemImageName: "questionmark.bubble")),
        ], intentIdentifiers: [])
    }
}
