import AlondraContracts
import Foundation

/// The reply field and the chosen option ID. Text goes to the option's declared text input;
/// nothing is inferred from labels or descriptions.
public struct QuestionDraft: Equatable, Sendable {
    public var text = ""
    public var selectedOption: String?

    public init() {}
}

public struct QuestionWorkIdentity: Hashable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String
    public let generation: UInt64

    public init(server: URL, deviceID: String, credentialRevision: String, generation: UInt64) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
        self.generation = generation
    }
}

/// A question or action request as the server projected it, with its own answer action.
public struct DomainQuestion: Equatable, Sendable, Identifiable {
    public let item: DomainV2.InboxItem
    public let interaction: DomainV2.Interaction

    public init?(item: DomainV2.InboxItem, in workspace: DomainWorkspace) {
        guard let interaction = workspace.interaction(of: item) else { return nil }
        self.init(item: item, interaction: interaction)
    }

    public init?(item: DomainV2.InboxItem, interaction: DomainV2.Interaction) {
        switch interaction.payload {
        case .question, .actionRequest: break
        default: return nil
        }
        self.item = item
        self.interaction = interaction
    }

    public var id: DomainV2.InteractionId { interaction.id }
    public var title: String { item.title }
    public var body: String { interaction.body }
    public var blocking: Bool { interaction.blocking }
    public var attachments: [DomainV2.AttachmentRef] { interaction.attachments }
    public var isActionRequest: Bool {
        if case .actionRequest = interaction.payload { return true }
        return false
    }

    public var options: [DomainV2.QuestionOption] {
        switch interaction.payload {
        case .question(let options, _), .actionRequest(let options, _): options
        default: []
        }
    }

    public var answer: DomainV2.Answer? {
        switch interaction.payload {
        case .question(_, let answer), .actionRequest(_, let answer): answer
        default: nil
        }
    }

    public var answerAction: DomainV2.AvailableAction? {
        item.availableActions.first { $0.actionKind == .answerQuestion }
    }

    public var canAnswer: Bool { answerAction?.enabled == true }

    public func option(_ id: String?) -> DomainV2.QuestionOption? { options.first { $0.id == id } }

    /// The intent for this draft, or the form error that keeps it from being sent.
    public func intent(for draft: QuestionDraft) throws -> DomainIntent {
        guard let option = option(draft.selectedOption) else { throw DomainIntentError.optionRequired }
        return .answer(id, optionID: option.id, inputs: try AnswerForm(option: option).inputs(text: draft.text))
    }
}

extension AnswerForm {
    public var acceptsText: Bool { option.inputs.contains { $0.kind == .text } }
    public var requiresText: Bool { option.inputs.contains { $0.kind == .text && $0.required } }

    /// Written text belongs to the option's first text input. Text for an option that has no
    /// text input is refused rather than dropped.
    public func inputs(text: String) throws -> [String: DomainV2.InputValue] {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let spec = option.inputs.first(where: { $0.kind == .text }) else {
            if trimmed.isEmpty { return [:] }
            throw DomainIntentError.inputInvalid("text")
        }
        let inputs: [String: DomainV2.InputValue] = trimmed.isEmpty ? [:] : [spec.id: .text(value: trimmed)]
        return try validated(inputs)
    }
}
