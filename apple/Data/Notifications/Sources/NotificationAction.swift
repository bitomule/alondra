import AlondraAPIClient
import AlondraContracts
import AlondraDomain
import AlondraNotificationPayload
import Foundation

/// What the person chose on a notification. The push only names the item and the choice; the
/// command is built from the item as the Mac serves it when the action is tapped, so an old
/// push acts on current state or says the item is no longer pending.
public enum NotificationAction: Equatable, Sendable {
    case answer(interaction: String, option: String, text: String?)
    case moreContext(interaction: String)
    case markRead(interaction: String)

    public init?(item: NotificationPayload.Item, question: NotificationPayload.Question?, action: String, text: String?) {
        guard let interaction = item.interactionID else { return nil }
        switch (item.kind, action) {
        case (.question, NotificationPayload.Action.reply), (.actionRequest, NotificationPayload.Action.reply):
            guard let answer = text?.trimmingCharacters(in: .whitespacesAndNewlines), !answer.isEmpty,
                  let option = question?.replyOption else { return nil }
            self = .answer(interaction: interaction, option: option.id, text: answer)
        case (.question, NotificationPayload.Action.moreContext), (.actionRequest, NotificationPayload.Action.moreContext):
            self = .moreContext(interaction: interaction)
        case (.information, NotificationPayload.Action.markRead):
            self = .markRead(interaction: interaction)
        case (.question, let action) where action.hasPrefix(NotificationPayload.Action.optionPrefix),
             (.actionRequest, let action) where action.hasPrefix(NotificationPayload.Action.optionPrefix):
            let id = String(action.dropFirst(NotificationPayload.Action.optionPrefix.count))
            guard let option = question?.options.first(where: { $0.id == id }), option.textInput == nil else { return nil }
            self = .answer(interaction: interaction, option: option.id, text: nil)
        default:
            return nil
        }
    }

    public var interactionID: DomainV2.InteractionId {
        switch self {
        case .answer(let id, _, _), .moreContext(let id), .markRead(let id): DomainV2.InteractionId(rawValue: id)
        }
    }

    /// The command for the item as it is now, under a new command ID. Throws
    /// `NotificationActionError.noLongerPending` when the choice no longer applies.
    public func envelope(fresh lookup: DomainV2.InboxLookup, interaction: DomainV2.Interaction,
                         commandID: String) throws -> DomainV2.CommandEnvelope {
        let item = lookup.item
        guard item.pending, item.subject == .interaction(interactionId: interactionID), interaction.id == interactionID else {
            throw NotificationActionError.noLongerPending
        }
        func enabled(_ kind: DomainV2.ActionKind) throws -> DomainV2.AvailableAction {
            guard let action = item.availableActions.first(where: { $0.actionKind == kind && $0.enabled }) else {
                throw NotificationActionError.noLongerPending
            }
            return action
        }
        let command: DomainV2.Command
        let revision: DomainV2.Revision
        switch self {
        case let .answer(_, optionID, text):
            let action = try enabled(.answerQuestion)
            let options: [DomainV2.QuestionOption] = switch interaction.payload {
            case .question(let options, _), .actionRequest(let options, _): options
            default: []
            }
            guard let option = options.first(where: { $0.id == optionID }) else { throw NotificationActionError.noLongerPending }
            command = .answerQuestion(attachments: [], interactionId: interactionID, optionId: option.id,
                                      inputs: try AnswerForm(option: option).inputs(text: text ?? ""))
            revision = action.expectedRevision
        case .moreContext:
            _ = try enabled(.answerQuestion)
            command = .requestMoreContext(attachments: [], interactionId: interactionID, body: "")
            revision = lookup.revision
        case .markRead:
            revision = try enabled(.markRead).expectedRevision
            command = .markRead(interactionId: interactionID)
        }
        return DomainV2.CommandEnvelope(protocolVersion: DomainV2.protocolVersion, dataEpoch: lookup.dataEpoch,
                                        commandId: DomainV2.CommandId(rawValue: commandID), expectedRevision: revision,
                                        command: command)
    }
}

public enum NotificationActionError: Error, Equatable, Sendable {
    case noLongerPending
}

/// A notification action either reached the Mac with its result, or found the item already
/// decided, gone or changed so the choice no longer applies.
public enum NotificationActionResult: Equatable, Sendable {
    case sent(DomainCommandResult)
    case noLongerPending
}
