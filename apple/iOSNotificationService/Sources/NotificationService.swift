import Foundation
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
        guard let question = OptionQuestion(userInfo: content.userInfo) else {
            delivery.finish(category: nil)
            return
        }
        Task {
            let category = await QuestionCategories.register(question)
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

struct OptionQuestion: Sendable {
    static let maxOptions = 4
    private static let origins: Set<String> = ["ask", "ask_user_question"]

    let id: UInt64
    let options: [String]

    init?(userInfo: [AnyHashable: Any]) {
        guard let alondra = userInfo["alondra"] as? [String: Any],
              alondra["kind"] as? String == "question",
              let origin = alondra["origin"] as? String, Self.origins.contains(origin),
              let id = (alondra["id"] as? NSNumber)?.uint64Value,
              let raw = alondra["options"] as? [String] else { return nil }
        var seen = Set<String>()
        let options = raw
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty && seen.insert($0).inserted }
            .prefix(Self.maxOptions)
        guard !options.isEmpty else { return nil }
        self.id = id
        self.options = Array(options)
    }
}

enum QuestionCategories {
    static let prefix = "ALONDRA_Q_"
    static let optionPrefix = "OPT:"
    static let limit = 20

    private static let reply = "REPLY"
    private static let moreContext = "MORE_CONTEXT"
    private static let registrationPolls = 20
    private static let pollInterval: Duration = .milliseconds(50)

    /// `setNotificationCategories` registers asynchronously; handing the content back before
    /// the new category is visible shows the notification without its buttons.
    static func register(_ question: OptionQuestion) async -> String {
        let center = UNUserNotificationCenter.current()
        let identifier = prefix + String(question.id)
        let existing = await center.notificationCategories()
        let recent = existing
            .compactMap { category in questionID(category).map { (id: $0, category: category) } }
            .filter { $0.id != question.id }
            .sorted { $0.id < $1.id }
            .suffix(limit - 1)
            .map(\.category)
        let fixed = existing.filter { questionID($0) == nil }
        center.setNotificationCategories(fixed.union(recent).union([category(for: question, identifier: identifier)]))
        for _ in 0..<registrationPolls {
            if await center.notificationCategories().contains(where: { $0.identifier == identifier }) { break }
            try? await Task.sleep(for: pollInterval)
        }
        return identifier
    }

    private static func questionID(_ category: UNNotificationCategory) -> UInt64? {
        guard category.identifier.hasPrefix(prefix) else { return nil }
        return UInt64(category.identifier.dropFirst(prefix.count))
    }

    private static func category(for question: OptionQuestion, identifier: String) -> UNNotificationCategory {
        let options = question.options.map { label in
            UNNotificationAction(identifier: optionPrefix + label, title: label, options: [.authenticationRequired])
        }
        return UNNotificationCategory(identifier: identifier, actions: options + [
            UNTextInputNotificationAction(
                identifier: reply,
                title: "Responder",
                options: [.authenticationRequired],
                icon: UNNotificationActionIcon(systemImageName: "arrowshape.turn.up.left"),
                textInputButtonTitle: "Enviar",
                textInputPlaceholder: "Tu respuesta"
            ),
            UNNotificationAction(identifier: moreContext, title: "Necesito más contexto", icon: UNNotificationActionIcon(systemImageName: "questionmark.bubble")),
        ], intentIdentifiers: [])
    }
}
