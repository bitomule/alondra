import Foundation

extension DomainV2 {
    public struct Request: Codable, Equatable, Sendable {
        public var id: RequestId
        public var body: String
        public var closed: Bool

        public init(id: RequestId, body: String, closed: Bool) {
            self.id = id
            self.body = body
            self.closed = closed
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case body = "body"
            case closed = "closed"
        }
    }

    public struct Agent: Codable, Equatable, Sendable {
        public var id: AgentId
        public var name: String
        public var connected: Bool

        public init(id: AgentId, name: String, connected: Bool) {
            self.id = id
            self.name = name
            self.connected = connected
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case name = "name"
            case connected = "connected"
        }
    }

    public struct Task: Codable, Equatable, Sendable {
        public var id: TaskId
        public var requestId: RequestId
        public var agentId: AgentId?
        public var title: String
        public var state: TaskState
        public var revision: Revision
        public var started: Bool
        public var reviewPrs: Bool
        public var finalPresentation: InteractionId?
        public var prId: PrId?
        public var dependencies: [TaskId]
        public var stack: StackDependency?
        public var documents: [String: String]
        public var archived: Bool

        public init(id: TaskId, requestId: RequestId, agentId: AgentId?, title: String, state: TaskState, revision: Revision, started: Bool, reviewPrs: Bool, finalPresentation: InteractionId?, prId: PrId?, dependencies: [TaskId], stack: StackDependency?, documents: [String: String], archived: Bool) {
            self.id = id
            self.requestId = requestId
            self.agentId = agentId
            self.title = title
            self.state = state
            self.revision = revision
            self.started = started
            self.reviewPrs = reviewPrs
            self.finalPresentation = finalPresentation
            self.prId = prId
            self.dependencies = dependencies
            self.stack = stack
            self.documents = documents
            self.archived = archived
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case requestId = "request_id"
            case agentId = "agent_id"
            case title = "title"
            case state = "state"
            case revision = "revision"
            case started = "started"
            case reviewPrs = "review_prs"
            case finalPresentation = "final_presentation"
            case prId = "pr_id"
            case dependencies = "dependencies"
            case stack = "stack"
            case documents = "documents"
            case archived = "archived"
        }
    }

    public struct StackDependency: Codable, Equatable, Sendable {
        public var upstreamTask: TaskId
        public var presentationId: InteractionId
        public var prId: PrId
        public var head: String

        public init(upstreamTask: TaskId, presentationId: InteractionId, prId: PrId, head: String) {
            self.upstreamTask = upstreamTask
            self.presentationId = presentationId
            self.prId = prId
            self.head = head
        }

        enum CodingKeys: String, CodingKey {
            case upstreamTask = "upstream_task"
            case presentationId = "presentation_id"
            case prId = "pr_id"
            case head = "head"
        }
    }

    public struct StackReadiness: Codable, Equatable, Sendable {
        public var taskId: TaskId
        public var presentationId: InteractionId
        public var prId: PrId
        public var head: String
        public var ready: Bool

        public init(taskId: TaskId, presentationId: InteractionId, prId: PrId, head: String, ready: Bool) {
            self.taskId = taskId
            self.presentationId = presentationId
            self.prId = prId
            self.head = head
            self.ready = ready
        }

        enum CodingKeys: String, CodingKey {
            case taskId = "task_id"
            case presentationId = "presentation_id"
            case prId = "pr_id"
            case head = "head"
            case ready = "ready"
        }
    }

    public struct Document: Codable, Equatable, Sendable {
        public var id: DocumentId
        public var title: String
        public var currentRevision: DocumentRevisionId

        public init(id: DocumentId, title: String, currentRevision: DocumentRevisionId) {
            self.id = id
            self.title = title
            self.currentRevision = currentRevision
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case title = "title"
            case currentRevision = "current_revision"
        }
    }

    public struct DocumentRevision: Codable, Equatable, Sendable {
        public var id: DocumentRevisionId
        public var documentId: DocumentId
        public var number: Revision
        public var contentDigest: String
        public var contentReference: String

        public init(id: DocumentRevisionId, documentId: DocumentId, number: Revision, contentDigest: String, contentReference: String) {
            self.id = id
            self.documentId = documentId
            self.number = number
            self.contentDigest = contentDigest
            self.contentReference = contentReference
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case documentId = "document_id"
            case number = "number"
            case contentDigest = "content_digest"
            case contentReference = "content_reference"
        }
    }

    public struct DocumentReview: Codable, Equatable, Sendable {
        public var taskId: TaskId
        public var documentId: DocumentId
        public var decisions: [DocumentDecision]

        public init(taskId: TaskId, documentId: DocumentId, decisions: [DocumentDecision]) {
            self.taskId = taskId
            self.documentId = documentId
            self.decisions = decisions
        }

        enum CodingKeys: String, CodingKey {
            case taskId = "task_id"
            case documentId = "document_id"
            case decisions = "decisions"
        }
    }

    public struct DocumentDecision: Codable, Equatable, Sendable {
        public var revisionId: DocumentRevisionId
        public var presentationId: InteractionId?
        public var accepted: Bool
        public var userId: String
        public var atRevision: Revision

        public init(revisionId: DocumentRevisionId, presentationId: InteractionId?, accepted: Bool, userId: String, atRevision: Revision) {
            self.revisionId = revisionId
            self.presentationId = presentationId
            self.accepted = accepted
            self.userId = userId
            self.atRevision = atRevision
        }

        enum CodingKeys: String, CodingKey {
            case revisionId = "revision_id"
            case presentationId = "presentation_id"
            case accepted = "accepted"
            case userId = "user_id"
            case atRevision = "at_revision"
        }
    }

    public struct PullRequest: Codable, Equatable, Sendable {
        public var id: PrId
        public var taskId: TaskId
        public var host: String
        public var repository: String
        public var number: UInt64
        public var head: String
        public var base: String
        public var state: PrState
        public var mergedHead: String?
        public var allowedMethods: [MergeMethod]
        public var review: PullRequestReview?

        public init(id: PrId, taskId: TaskId, host: String, repository: String, number: UInt64, head: String, base: String, state: PrState, mergedHead: String?, allowedMethods: [MergeMethod], review: PullRequestReview?) {
            self.id = id
            self.taskId = taskId
            self.host = host
            self.repository = repository
            self.number = number
            self.head = head
            self.base = base
            self.state = state
            self.mergedHead = mergedHead
            self.allowedMethods = allowedMethods
            self.review = review
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case taskId = "task_id"
            case host = "host"
            case repository = "repository"
            case number = "number"
            case head = "head"
            case base = "base"
            case state = "state"
            case mergedHead = "merged_head"
            case allowedMethods = "allowed_methods"
            case review = "review"
        }
    }

    public struct PullRequestReview: Codable, Equatable, Sendable {
        public var decisions: [PrDecision]

        public init(decisions: [PrDecision]) {
            self.decisions = decisions
        }

        enum CodingKeys: String, CodingKey {
            case decisions = "decisions"
        }
    }

    public struct PrDecision: Codable, Equatable, Sendable {
        public var presentationId: InteractionId
        public var head: String
        public var accepted: Bool
        public var userId: String
        public var atRevision: Revision

        public init(presentationId: InteractionId, head: String, accepted: Bool, userId: String, atRevision: Revision) {
            self.presentationId = presentationId
            self.head = head
            self.accepted = accepted
            self.userId = userId
            self.atRevision = atRevision
        }

        enum CodingKeys: String, CodingKey {
            case presentationId = "presentation_id"
            case head = "head"
            case accepted = "accepted"
            case userId = "user_id"
            case atRevision = "at_revision"
        }
    }

    public struct InputSpec: Codable, Equatable, Sendable {
        public var id: String
        public var kind: InputKind
        public var required: Bool

        public init(id: String, kind: InputKind, required: Bool) {
            self.id = id
            self.kind = kind
            self.required = required
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case kind = "kind"
            case required = "required"
        }
    }

    public struct QuestionOption: Codable, Equatable, Sendable {
        public var id: String
        public var label: String
        public var inputs: [InputSpec]

        public init(id: String, label: String, inputs: [InputSpec]) {
            self.id = id
            self.label = label
            self.inputs = inputs
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case label = "label"
            case inputs = "inputs"
        }
    }

    public struct Answer: Codable, Equatable, Sendable {
        public var optionId: String
        public var inputs: [String: InputValue]
        public var responder: String
        public var atRevision: Revision

        public init(optionId: String, inputs: [String: InputValue], responder: String, atRevision: Revision) {
            self.optionId = optionId
            self.inputs = inputs
            self.responder = responder
            self.atRevision = atRevision
        }

        enum CodingKeys: String, CodingKey {
            case optionId = "option_id"
            case inputs = "inputs"
            case responder = "responder"
            case atRevision = "at_revision"
        }
    }

    public struct Acceptance: Codable, Equatable, Sendable {
        public var userId: String
        public var atRevision: Revision
        public var valid: Bool

        public init(userId: String, atRevision: Revision, valid: Bool) {
            self.userId = userId
            self.atRevision = atRevision
            self.valid = valid
        }

        enum CodingKeys: String, CodingKey {
            case userId = "user_id"
            case atRevision = "at_revision"
            case valid = "valid"
        }
    }

    public struct Feedback: Codable, Equatable, Sendable {
        public var userId: String
        public var body: String
        public var atRevision: Revision

        public init(userId: String, body: String, atRevision: Revision) {
            self.userId = userId
            self.body = body
            self.atRevision = atRevision
        }

        enum CodingKeys: String, CodingKey {
            case userId = "user_id"
            case body = "body"
            case atRevision = "at_revision"
        }
    }

    public struct Interaction: Codable, Equatable, Sendable {
        public var id: InteractionId
        public var scope: Scope
        public var author: Author
        public var recipient: Recipient
        public var body: String
        public var blocking: Bool
        public var subject: InteractionSubject?
        public var payload: InteractionPayload
        public var revision: Revision
        public var order: Revision

        public init(id: InteractionId, scope: Scope, author: Author, recipient: Recipient, body: String, blocking: Bool, subject: InteractionSubject?, payload: InteractionPayload, revision: Revision, order: Revision) {
            self.id = id
            self.scope = scope
            self.author = author
            self.recipient = recipient
            self.body = body
            self.blocking = blocking
            self.subject = subject
            self.payload = payload
            self.revision = revision
            self.order = order
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case scope = "scope"
            case author = "author"
            case recipient = "recipient"
            case body = "body"
            case blocking = "blocking"
            case subject = "subject"
            case payload = "payload"
            case revision = "revision"
            case order = "order"
        }
    }

    public struct MergeAuthorization: Codable, Equatable, Sendable {
        public var id: AuthorizationId
        public var taskId: TaskId
        public var prId: PrId
        public var presentationId: InteractionId
        public var head: String
        public var method: MergeMethod
        public var userId: String
        public var valid: Bool
        public var attempt: MergeAttemptState
        public var claimedBy: AgentId?
        public var atRevision: Revision

        public init(id: AuthorizationId, taskId: TaskId, prId: PrId, presentationId: InteractionId, head: String, method: MergeMethod, userId: String, valid: Bool, attempt: MergeAttemptState, claimedBy: AgentId?, atRevision: Revision) {
            self.id = id
            self.taskId = taskId
            self.prId = prId
            self.presentationId = presentationId
            self.head = head
            self.method = method
            self.userId = userId
            self.valid = valid
            self.attempt = attempt
            self.claimedBy = claimedBy
            self.atRevision = atRevision
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case taskId = "task_id"
            case prId = "pr_id"
            case presentationId = "presentation_id"
            case head = "head"
            case method = "method"
            case userId = "user_id"
            case valid = "valid"
            case attempt = "attempt"
            case claimedBy = "claimed_by"
            case atRevision = "at_revision"
        }
    }

    public struct CommandEnvelope: Codable, Equatable, Sendable {
        public var protocolVersion: UInt32
        public var dataEpoch: DataEpoch
        public var commandId: CommandId
        public var expectedRevision: Revision
        public var command: Command

        public init(protocolVersion: UInt32, dataEpoch: DataEpoch, commandId: CommandId, expectedRevision: Revision, command: Command) {
            self.protocolVersion = protocolVersion
            self.dataEpoch = dataEpoch
            self.commandId = commandId
            self.expectedRevision = expectedRevision
            self.command = command
        }

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case dataEpoch = "data_epoch"
            case commandId = "command_id"
            case expectedRevision = "expected_revision"
            case command = "command"
        }
    }

    public struct CommandOutcome: Codable, Equatable, Sendable {
        public var revision: Revision
        public var changed: Bool
        public var effects: [DomainEffect]
        public var createdIds: [String]

        public init(revision: Revision, changed: Bool, effects: [DomainEffect], createdIds: [String]) {
            self.revision = revision
            self.changed = changed
            self.effects = effects
            self.createdIds = createdIds
        }

        enum CodingKeys: String, CodingKey {
            case revision = "revision"
            case changed = "changed"
            case effects = "effects"
            case createdIds = "created_ids"
        }
    }

    public struct DomainError: Codable, Equatable, Sendable {
        public var code: String
        public var message: String
        public var currentRevision: Revision?
        public var subject: String?

        public init(code: String, message: String, currentRevision: Revision?, subject: String?) {
            self.code = code
            self.message = message
            self.currentRevision = currentRevision
            self.subject = subject
        }

        enum CodingKeys: String, CodingKey {
            case code = "code"
            case message = "message"
            case currentRevision = "current_revision"
            case subject = "subject"
        }
    }

}
