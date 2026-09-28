import AlondraAPIClient
import AlondraContracts
import AlondraNotificationPayload
import CryptoKit
import Foundation

/// Headless notification action boundary. Each action is one v2 command decided against the
/// epoch and revision the push carried; the server refuses it if the item changed since.
public enum NotificationAction: Equatable, Sendable {
    case answer(interaction: String, option: String, input: String?, text: String?)
    case moreContext(interaction: String)
    case markRead(interaction: String)

    public init?(item: NotificationPayload.Item, question: NotificationPayload.Question?, action: String, text: String?) {
        guard let interaction = item.interactionID else { return nil }
        switch (item.kind, action) {
        case (.question, NotificationPayload.Action.reply), (.actionRequest, NotificationPayload.Action.reply):
            guard let answer = text?.trimmingCharacters(in: .whitespacesAndNewlines), !answer.isEmpty,
                  let option = question?.replyOption else { return nil }
            self = .answer(interaction: interaction, option: option.id, input: option.textInput, text: answer)
        case (.question, NotificationPayload.Action.moreContext), (.actionRequest, NotificationPayload.Action.moreContext):
            self = .moreContext(interaction: interaction)
        case (.information, NotificationPayload.Action.markRead):
            self = .markRead(interaction: interaction)
        case (.question, let action) where action.hasPrefix(NotificationPayload.Action.optionPrefix),
             (.actionRequest, let action) where action.hasPrefix(NotificationPayload.Action.optionPrefix):
            let id = String(action.dropFirst(NotificationPayload.Action.optionPrefix.count))
            guard let option = question?.options.first(where: { $0.id == id }), option.textInput == nil else { return nil }
            self = .answer(interaction: interaction, option: option.id, input: nil, text: nil)
        default:
            return nil
        }
    }

    public var command: DomainV2.Command {
        switch self {
        case let .answer(interaction, option, input, text):
            let inputs: [String: DomainV2.InputValue] = if let input, let text { [input: .text(value: text)] } else { [:] }
            return .answerQuestion(attachments: [], interactionId: .init(rawValue: interaction), optionId: option, inputs: inputs)
        case .moreContext(let interaction):
            return .requestMoreContext(attachments: [], interactionId: .init(rawValue: interaction), body: "")
        case .markRead(let interaction):
            return .markRead(interactionId: .init(rawValue: interaction))
        }
    }

    /// The command ID is derived from the push and the choice, so tapping the same action
    /// again retries the same command and never applies it twice.
    public func envelope(for item: NotificationPayload.Item) throws -> DomainV2.CommandEnvelope {
        guard let epoch = item.epoch, let revision = item.expectedRevision else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Notification without a decision revision",
                                       currentRevision: nil, subject: item.itemID)
        }
        let command = command
        let identity = try DomainV2.encode(command) + Data("|\(epoch)|\(revision)".utf8)
        let digest = SHA256.hash(data: identity).prefix(16).map { String(format: "%02x", $0) }.joined()
        return DomainV2.CommandEnvelope(protocolVersion: DomainV2.protocolVersion, dataEpoch: .init(rawValue: epoch),
                                        commandId: .init(rawValue: "notification-" + digest), expectedRevision: revision,
                                        command: command)
    }
}
