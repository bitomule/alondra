import Foundation

/// Why a message goes to an agent: the daemon words it for the agent from this.
public enum AgentMessageKind: String, Encodable, Sendable {
    case message
    case reply
    case moreContext = "more_context"
    case prChanges = "pr_changes"
    case prQuestion = "pr_question"
}

/// `POST /agents/{id}/messages`: text typed into the agent's session.
public struct AgentMessage: Encodable, Equatable, Sendable {
    public var kind: AgentMessageKind
    public var text: String
    public var attachments: [DomainV2.AttachmentRef]
    public var interactionId: DomainV2.InteractionId?
    public var prId: DomainV2.PrId?
    public var taskId: DomainV2.TaskId?
    /// Marks `interactionId` read in the same step, for a reply to information.
    public var markRead: Bool

    public init(kind: AgentMessageKind = .message, text: String, attachments: [DomainV2.AttachmentRef] = [],
                interactionId: DomainV2.InteractionId? = nil, prId: DomainV2.PrId? = nil, taskId: DomainV2.TaskId? = nil,
                markRead: Bool = false) {
        self.kind = kind
        self.text = text
        self.attachments = attachments
        self.interactionId = interactionId
        self.prId = prId
        self.taskId = taskId
        self.markRead = markRead
    }

    enum CodingKeys: String, CodingKey {
        case kind, text, attachments
        case interactionId = "interaction_id", prId = "pr_id", taskId = "task_id", markRead = "mark_read"
    }
}

/// How far a message got: typed into the session (`handed`), maybe typed (`uncertain`), or held
/// for an interactive terminal session until its turn ends (`interactive` with a `note`).
public struct AgentMessageReceipt: Decodable, Equatable, Sendable {
    public let deliveryState: String?
    public let interactive: Bool?
    public let note: String?

    public var uncertain: Bool { deliveryState == "uncertain" }
}

/// `POST /requests`: a request with what the plain `create_request` command cannot carry.
public struct NewRequestSubmission: Encodable, Equatable, Sendable {
    public let requestID: String
    public let text: String
    public let project: String?
    public let options: [String: OptionChoice]
    public let issue: String?
    public let attachments: [DomainV2.AttachmentRef]

    public init(requestID: String, text: String, project: String?, options: [String: OptionChoice], issue: String?,
                attachments: [DomainV2.AttachmentRef]) {
        self.requestID = requestID
        self.text = text
        self.project = project
        self.options = options
        self.issue = issue
        self.attachments = attachments
    }

    enum CodingKeys: String, CodingKey {
        case requestID = "request_id", text, project, options, issue, attachments
    }
}
