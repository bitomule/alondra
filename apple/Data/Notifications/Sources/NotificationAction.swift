import AlondraNotificationPayload
import AlondraCommands
import AlondraAPIClient
import Foundation

/// Headless notification action boundary. The app owns the background task and
/// obtains a server-scoped signed client; this layer decides the daemon command.
public enum NotificationAction: Equatable, Sendable {
    case answer(question: UInt64, text: String, option: String?)
    case moreContext(question: UInt64)
    case markRead(report: UInt64)

    public var commandKind: CommandKind {
        switch self {
        case .answer(let question, _, _): .answer(question)
        case .moreContext(let question): .moreContext(question)
        case .markRead(let report): .markRead(report)
        }
    }

    public init?(item: NotificationPayload.Item, action: String, text: String?) {
        switch (item.kind, action) {
        case (.question, NotificationPayload.Action.reply):
            guard let answer = text?.trimmingCharacters(in: .whitespacesAndNewlines), !answer.isEmpty else { return nil }
            self = .answer(question: item.id, text: answer, option: nil)
        case (.question, NotificationPayload.Action.moreContext):
            self = .moreContext(question: item.id)
        case (.question, NotificationPayload.Action.allow):
            self = .answer(question: item.id, text: "Allow", option: "Allow")
        case (.question, NotificationPayload.Action.deny):
            self = .answer(question: item.id, text: "Deny", option: "Deny")
        case (.question, NotificationPayload.Action.approve):
            self = .answer(question: item.id, text: "Approve", option: "Approve")
        case (.report, NotificationPayload.Action.markRead):
            self = .markRead(report: item.id)
        case (.question, let action) where action.hasPrefix(NotificationPayload.Action.optionPrefix):
            let label = String(action.dropFirst(NotificationPayload.Action.optionPrefix.count))
            guard !label.isEmpty else { return nil }
            self = .answer(question: item.id, text: label, option: label)
        default:
            return nil
        }
    }

    public func execute(using client: AlondraAPIClient, operationID: UUID) async throws {
        switch self {
        case let .answer(question, text, option): try await client.answer(question, text: text, option: option, attachments: [], operationID: operationID.uuidString)
        case let .moreContext(question): _ = try await client.send(AlondraEndpoints.moreContext(question, note: "", attachments: []), operationID: operationID.uuidString)
        case let .markRead(report): try await client.markRead(report, operationID: operationID.uuidString)
        }
    }
}
