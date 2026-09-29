import Foundation

extension DomainV2 {
    public struct DomainProjection: Codable, Equatable, Sendable {
        public var protocolVersion: UInt32
        public var dataEpoch: DataEpoch
        public var revision: Revision
        public var inbox: [InboxItem]
        public var counts: InboxCounts
        public var tasks: [Task]
        public var agents: [Agent]
        public var requests: [Request]
        public var interactions: [Interaction]
        public var pullRequests: [PullRequest]
        public var documents: [Document]
        public var documentRevisions: [DocumentRevision]
        public var documentReviews: [DocumentReview]
        public var mergeAuthorizations: [MergeAuthorization]
        public var stackReadiness: [StackReadiness]
        public var supersededAcceptances: [SupersededAcceptance]

        public init(protocolVersion: UInt32, dataEpoch: DataEpoch, revision: Revision, inbox: [InboxItem], counts: InboxCounts, tasks: [Task], agents: [Agent], requests: [Request], interactions: [Interaction], pullRequests: [PullRequest], documents: [Document], documentRevisions: [DocumentRevision], documentReviews: [DocumentReview], mergeAuthorizations: [MergeAuthorization], stackReadiness: [StackReadiness], supersededAcceptances: [SupersededAcceptance] = []) {
            self.protocolVersion = protocolVersion
            self.dataEpoch = dataEpoch
            self.revision = revision
            self.inbox = inbox
            self.counts = counts
            self.tasks = tasks
            self.agents = agents
            self.requests = requests
            self.interactions = interactions
            self.pullRequests = pullRequests
            self.documents = documents
            self.documentRevisions = documentRevisions
            self.documentReviews = documentReviews
            self.mergeAuthorizations = mergeAuthorizations
            self.stackReadiness = stackReadiness
            self.supersededAcceptances = supersededAcceptances
        }

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case dataEpoch = "data_epoch"
            case revision = "revision"
            case inbox = "inbox"
            case counts = "counts"
            case tasks = "tasks"
            case agents = "agents"
            case requests = "requests"
            case interactions = "interactions"
            case pullRequests = "pull_requests"
            case documents = "documents"
            case documentRevisions = "document_revisions"
            case documentReviews = "document_reviews"
            case mergeAuthorizations = "merge_authorizations"
            case stackReadiness = "stack_readiness"
            case supersededAcceptances = "superseded_acceptances"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            protocolVersion = try container.decode(UInt32.self, forKey: .protocolVersion)
            dataEpoch = try container.decode(DataEpoch.self, forKey: .dataEpoch)
            revision = try container.decode(Revision.self, forKey: .revision)
            inbox = try container.decode([InboxItem].self, forKey: .inbox)
            counts = try container.decode(InboxCounts.self, forKey: .counts)
            tasks = try container.decode([Task].self, forKey: .tasks)
            agents = try container.decode([Agent].self, forKey: .agents)
            requests = try container.decode([Request].self, forKey: .requests)
            interactions = try container.decode([Interaction].self, forKey: .interactions)
            pullRequests = try container.decode([PullRequest].self, forKey: .pullRequests)
            documents = try container.decode([Document].self, forKey: .documents)
            documentRevisions = try container.decode([DocumentRevision].self, forKey: .documentRevisions)
            documentReviews = try container.decode([DocumentReview].self, forKey: .documentReviews)
            mergeAuthorizations = try container.decode([MergeAuthorization].self, forKey: .mergeAuthorizations)
            stackReadiness = try container.decode([StackReadiness].self, forKey: .stackReadiness)
            supersededAcceptances = try container.decodeIfPresent([SupersededAcceptance].self, forKey: .supersededAcceptances) ?? []
        }
    }

    public struct SupersededAcceptance: Codable, Equatable, Hashable, Sendable {
        public var taskId: TaskId
        public var presentationId: InteractionId
        public var userId: String
        public var acceptedAt: Revision

        public init(taskId: TaskId, presentationId: InteractionId, userId: String, acceptedAt: Revision) {
            self.taskId = taskId
            self.presentationId = presentationId
            self.userId = userId
            self.acceptedAt = acceptedAt
        }

        enum CodingKeys: String, CodingKey {
            case taskId = "task_id"
            case presentationId = "presentation_id"
            case userId = "user_id"
            case acceptedAt = "accepted_at"
        }
    }

    public struct InboxCounts: Codable, Equatable, Sendable {
        public var roots: UInt64
        public var pending: UInt64
        public var unreadInformation: UInt64

        public init(roots: UInt64, pending: UInt64, unreadInformation: UInt64) {
            self.roots = roots
            self.pending = pending
            self.unreadInformation = unreadInformation
        }

        enum CodingKeys: String, CodingKey {
            case roots = "roots"
            case pending = "pending"
            case unreadInformation = "unread_information"
        }
    }

    public struct InboxItem: Codable, Equatable, Sendable {
        public var id: String
        public var kind: String
        public var subject: InboxSubject
        public var title: String
        public var body: String?
        public var taskId: TaskId?
        public var requestId: RequestId?
        public var agentId: AgentId?
        public var revision: Revision
        public var order: Revision
        public var pending: Bool
        public var children: [InboxItem]
        public var availableActions: [AvailableAction]

        public init(id: String, kind: String, subject: InboxSubject, title: String, body: String?, taskId: TaskId?, requestId: RequestId?, agentId: AgentId?, revision: Revision, order: Revision, pending: Bool, children: [InboxItem], availableActions: [AvailableAction]) {
            self.id = id
            self.kind = kind
            self.subject = subject
            self.title = title
            self.body = body
            self.taskId = taskId
            self.requestId = requestId
            self.agentId = agentId
            self.revision = revision
            self.order = order
            self.pending = pending
            self.children = children
            self.availableActions = availableActions
        }

        enum CodingKeys: String, CodingKey {
            case id = "id"
            case kind = "kind"
            case subject = "subject"
            case title = "title"
            case body = "body"
            case taskId = "task_id"
            case requestId = "request_id"
            case agentId = "agent_id"
            case revision = "revision"
            case order = "order"
            case pending = "pending"
            case children = "children"
            case availableActions = "available_actions"
        }
    }

    public struct AvailableAction: Codable, Equatable, Sendable {
        public var kind: String
        public var enabled: Bool
        public var disabledReason: String?
        public var expectedRevision: Revision
        public var presentationId: InteractionId?
        public var expectedHead: String?

        public init(kind: String, enabled: Bool, disabledReason: String?, expectedRevision: Revision, presentationId: InteractionId?, expectedHead: String?) {
            self.kind = kind
            self.enabled = enabled
            self.disabledReason = disabledReason
            self.expectedRevision = expectedRevision
            self.presentationId = presentationId
            self.expectedHead = expectedHead
        }

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case enabled = "enabled"
            case disabledReason = "disabled_reason"
            case expectedRevision = "expected_revision"
            case presentationId = "presentation_id"
            case expectedHead = "expected_head"
        }
    }

    public enum InboxSubject: Codable, Equatable, Sendable {
        case interaction(interactionId: InteractionId)
        case pullRequest(prId: PrId)
        case documentReview(taskId: TaskId, documentId: DocumentId, revisionId: DocumentRevisionId)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case interactionId = "interaction_id"
            case prId = "pr_id"
            case taskId = "task_id"
            case documentId = "document_id"
            case revisionId = "revision_id"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "interaction": self = .interaction(interactionId: try container.decode(InteractionId.self, forKey: .interactionId))
            case "pull_request": self = .pullRequest(prId: try container.decode(PrId.self, forKey: .prId))
            case "document_review": self = .documentReview(taskId: try container.decode(TaskId.self, forKey: .taskId), documentId: try container.decode(DocumentId.self, forKey: .documentId), revisionId: try container.decode(DocumentRevisionId.self, forKey: .revisionId))
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 InboxSubject kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .interaction(interactionId):
                try container.encode("interaction", forKey: .kind)
                try container.encode(interactionId, forKey: .interactionId)
            case let .pullRequest(prId):
                try container.encode("pull_request", forKey: .kind)
                try container.encode(prId, forKey: .prId)
            case let .documentReview(taskId, documentId, revisionId):
                try container.encode("document_review", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(documentId, forKey: .documentId)
                try container.encode(revisionId, forKey: .revisionId)
            }
        }
    }

}
