import Foundation

extension DomainV2 {
    public enum Author: Codable, Equatable, Sendable {
        case agent(agentId: AgentId)
        case orchestrator(generation: UInt64)
        case system
        case unknown(kind: String)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case agentId = "agent_id"
            case generation = "generation"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "agent": self = .agent(agentId: try container.decode(AgentId.self, forKey: .agentId))
            case "orchestrator": self = .orchestrator(generation: try container.decode(UInt64.self, forKey: .generation))
            case "system": self = .system
            case let kind: self = .unknown(kind: kind)
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .agent(agentId):
                try container.encode("agent", forKey: .kind)
                try container.encode(agentId, forKey: .agentId)
            case let .orchestrator(generation):
                try container.encode("orchestrator", forKey: .kind)
                try container.encode(generation, forKey: .generation)
            case .system:
                try container.encode("system", forKey: .kind)
            case let .unknown(kind):
                try container.encode(kind, forKey: .kind)
            }
        }
    }

    public enum Recipient: Codable, Equatable, Sendable {
        case user
        case agent(agentId: AgentId)
        case orchestrator

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case agentId = "agent_id"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "user": self = .user
            case "agent": self = .agent(agentId: try container.decode(AgentId.self, forKey: .agentId))
            case "orchestrator": self = .orchestrator
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 Recipient kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case .user:
                try container.encode("user", forKey: .kind)
            case let .agent(agentId):
                try container.encode("agent", forKey: .kind)
                try container.encode(agentId, forKey: .agentId)
            case .orchestrator:
                try container.encode("orchestrator", forKey: .kind)
            }
        }
    }

    public enum Scope: Codable, Equatable, Sendable {
        case task(taskId: TaskId)
        case orchestrator(requestId: RequestId?)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case taskId = "task_id"
            case requestId = "request_id"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "task": self = .task(taskId: try container.decode(TaskId.self, forKey: .taskId))
            case "orchestrator": self = .orchestrator(requestId: try container.decodeIfPresent(RequestId.self, forKey: .requestId))
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 Scope kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .task(taskId):
                try container.encode("task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
            case let .orchestrator(requestId):
                try container.encode("orchestrator", forKey: .kind)
                try container.encodeIfPresent(requestId, forKey: .requestId)
            }
        }
    }

    public enum TaskState: String, Codable, Equatable, Sendable {
        case notStarted = "not_started"
        case running = "running"
        case needsYou = "needs_you"
        case inReview = "in_review"
        case waitingDependency = "waiting_dependency"
        case closing = "closing"
        case done = "done"
        case cancelled = "cancelled"
    }

    public enum PrState: String, Codable, Equatable, Sendable {
        case open = "open"
        case closed = "closed"
        case merged = "merged"
    }

    public enum MergeMethod: String, Codable, Equatable, Sendable {
        case merge = "merge"
        case squash = "squash"
        case rebase = "rebase"
    }

    public enum DeliveryTarget: Codable, Equatable, Sendable {
        case text(text: String)
        case documentRevision(documentId: DocumentId, revisionId: DocumentRevisionId)
        case pullRequest(prId: PrId, head: String)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case text = "text"
            case documentId = "document_id"
            case revisionId = "revision_id"
            case prId = "pr_id"
            case head = "head"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "text": self = .text(text: try container.decode(String.self, forKey: .text))
            case "document_revision": self = .documentRevision(documentId: try container.decode(DocumentId.self, forKey: .documentId), revisionId: try container.decode(DocumentRevisionId.self, forKey: .revisionId))
            case "pull_request": self = .pullRequest(prId: try container.decode(PrId.self, forKey: .prId), head: try container.decode(String.self, forKey: .head))
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 DeliveryTarget kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .text(text):
                try container.encode("text", forKey: .kind)
                try container.encode(text, forKey: .text)
            case let .documentRevision(documentId, revisionId):
                try container.encode("document_revision", forKey: .kind)
                try container.encode(documentId, forKey: .documentId)
                try container.encode(revisionId, forKey: .revisionId)
            case let .pullRequest(prId, head):
                try container.encode("pull_request", forKey: .kind)
                try container.encode(prId, forKey: .prId)
                try container.encode(head, forKey: .head)
            }
        }
    }

    public enum InteractionSubject: Codable, Equatable, Sendable {
        case pullRequest(prId: PrId)
        case documentReview(documentId: DocumentId, revisionId: DocumentRevisionId)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case prId = "pr_id"
            case documentId = "document_id"
            case revisionId = "revision_id"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "pull_request": self = .pullRequest(prId: try container.decode(PrId.self, forKey: .prId))
            case "document_review": self = .documentReview(documentId: try container.decode(DocumentId.self, forKey: .documentId), revisionId: try container.decode(DocumentRevisionId.self, forKey: .revisionId))
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 InteractionSubject kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .pullRequest(prId):
                try container.encode("pull_request", forKey: .kind)
                try container.encode(prId, forKey: .prId)
            case let .documentReview(documentId, revisionId):
                try container.encode("document_review", forKey: .kind)
                try container.encode(documentId, forKey: .documentId)
                try container.encode(revisionId, forKey: .revisionId)
            }
        }
    }

    public enum InputKind: String, Codable, Equatable, Sendable {
        case text = "text"
        case boolean = "boolean"
        case integer = "integer"
    }

    public enum InputValue: Codable, Equatable, Sendable {
        case text(value: String)
        case boolean(value: Bool)
        case integer(value: Int64)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case value = "value"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "text": self = .text(value: try container.decode(String.self, forKey: .value))
            case "boolean": self = .boolean(value: try container.decode(Bool.self, forKey: .value))
            case "integer": self = .integer(value: try container.decode(Int64.self, forKey: .value))
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 InputValue kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .text(value):
                try container.encode("text", forKey: .kind)
                try container.encode(value, forKey: .value)
            case let .boolean(value):
                try container.encode("boolean", forKey: .kind)
                try container.encode(value, forKey: .value)
            case let .integer(value):
                try container.encode("integer", forKey: .kind)
                try container.encode(value, forKey: .value)
            }
        }
    }

    public enum InteractionPayload: Codable, Equatable, Sendable {
        case information(readBy: [String])
        case question(options: [QuestionOption], answer: Answer?)
        case actionRequest(options: [QuestionOption], answer: Answer?)
        case delivery(targets: [DeliveryTarget], isFinal: Bool, acceptance: Acceptance?, feedback: [Feedback])

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case readBy = "read_by"
            case options = "options"
            case answer = "answer"
            case targets = "targets"
            case isFinal = "is_final"
            case acceptance = "acceptance"
            case feedback = "feedback"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "information": self = .information(readBy: try container.decode([String].self, forKey: .readBy))
            case "question": self = .question(options: try container.decode([QuestionOption].self, forKey: .options), answer: try container.decodeIfPresent(Answer.self, forKey: .answer))
            case "action_request": self = .actionRequest(options: try container.decode([QuestionOption].self, forKey: .options), answer: try container.decodeIfPresent(Answer.self, forKey: .answer))
            case "delivery": self = .delivery(targets: try container.decode([DeliveryTarget].self, forKey: .targets), isFinal: try container.decode(Bool.self, forKey: .isFinal), acceptance: try container.decodeIfPresent(Acceptance.self, forKey: .acceptance), feedback: try container.decode([Feedback].self, forKey: .feedback))
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 InteractionPayload kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .information(readBy):
                try container.encode("information", forKey: .kind)
                try container.encode(readBy, forKey: .readBy)
            case let .question(options, answer):
                try container.encode("question", forKey: .kind)
                try container.encode(options, forKey: .options)
                try container.encodeIfPresent(answer, forKey: .answer)
            case let .actionRequest(options, answer):
                try container.encode("action_request", forKey: .kind)
                try container.encode(options, forKey: .options)
                try container.encodeIfPresent(answer, forKey: .answer)
            case let .delivery(targets, isFinal, acceptance, feedback):
                try container.encode("delivery", forKey: .kind)
                try container.encode(targets, forKey: .targets)
                try container.encode(isFinal, forKey: .isFinal)
                try container.encodeIfPresent(acceptance, forKey: .acceptance)
                try container.encode(feedback, forKey: .feedback)
            }
        }
    }

    public enum MergeAttemptState: String, Codable, Equatable, Sendable {
        case unclaimed = "unclaimed"
        case claimed = "claimed"
        case uncertain = "uncertain"
        case confirmed = "confirmed"
        case failed = "failed"
    }

    public enum Command: Codable, Equatable, Sendable {
        case createRequest(requestId: RequestId, body: String)
        case closeRequest(requestId: RequestId, answer: String? = nil, withdrawn: Bool = false, title: String? = nil)
        case retitleTask(taskId: TaskId, title: String)
        case withdrawQuestion(interactionId: InteractionId)
        case registerAgent(agentId: AgentId, name: String)
        case observeAgent(agentId: AgentId, connected: Bool)
        case advanceOrchestratorGeneration(generation: UInt64)
        case createTask(taskId: TaskId, requestId: RequestId, title: String, agentId: AgentId?, reviewPrs: Bool, dependencies: [TaskId], stack: StackDependency?)
        case assignTask(taskId: TaskId, agentId: AgentId)
        case setDependencies(taskId: TaskId, dependencies: [TaskId], stack: StackDependency?)
        case startTask(taskId: TaskId)
        case publishInformation(attachments: [AttachmentRef], interactionId: InteractionId, scope: Scope, recipient: Recipient, body: String)
        case askQuestion(attachments: [AttachmentRef], interactionId: InteractionId, scope: Scope, recipient: Recipient, body: String, blocking: Bool, options: [QuestionOption])
        case requestAction(attachments: [AttachmentRef], interactionId: InteractionId, scope: Scope, recipient: Recipient, body: String, blocking: Bool, options: [QuestionOption])
        case markRead(interactionId: InteractionId)
        case answerQuestion(attachments: [AttachmentRef], interactionId: InteractionId, optionId: String, inputs: [String: InputValue])
        case createDocument(taskId: TaskId, documentId: DocumentId, title: String, revisionId: DocumentRevisionId, contentDigest: String, contentReference: String, role: String)
        case reviseDocument(taskId: TaskId, documentId: DocumentId, revisionId: DocumentRevisionId, contentDigest: String, contentReference: String)
        case linkTaskDocument(taskId: TaskId, documentId: DocumentId, role: String)
        case decideDocumentReview(taskId: TaskId, documentId: DocumentId, revisionId: DocumentRevisionId, accepted: Bool)
        case registerPr(taskId: TaskId, prId: PrId, host: String, repository: String, number: UInt64, head: String, base: String, allowedMethods: [MergeMethod])
        case linkInteractionSubject(interactionId: InteractionId, subject: InteractionSubject)
        case presentResult(attachments: [AttachmentRef], taskId: TaskId, interactionId: InteractionId, body: String, targets: [DeliveryTarget], isFinal: Bool)
        case submitFeedback(attachments: [AttachmentRef], presentationId: InteractionId, targets: [DeliveryTarget], body: String)
        case acceptDelivery(presentationId: InteractionId, targets: [DeliveryTarget])
        case authorizeMerge(authorizationId: AuthorizationId, presentationId: InteractionId, prId: PrId, expectedHead: String, method: MergeMethod)
        case claimMergeAuthorization(authorizationId: AuthorizationId, remoteHead: String, remoteBase: String, baseVerified: Bool)
        case observeMergeAttempt(authorizationId: AuthorizationId, state: MergeAttemptState)
        case observePrState(prId: PrId, head: String, base: String, state: PrState, mergedHead: String?)
        case publishStackReadiness(taskId: TaskId, presentationId: InteractionId, prId: PrId, head: String, ready: Bool)
        case completeTask(taskId: TaskId, presentationId: InteractionId)
        case cancelTask(taskId: TaskId)
        case reopenTask(taskId: TaskId)
        case archiveTask(taskId: TaskId, archived: Bool)
        // Client contracts the daemon does not accept yet (docs/domain-v2-apple-client-contracts.md).
        // Each keeps a v1 feature on the v2 command service; today's daemon rejects them explicitly.
        case createRequestWithContext(requestId: RequestId, body: String, project: String?, options: [String: OptionChoice], issue: String?, attachments: [AttachmentRef])
        case requestMoreContext(attachments: [AttachmentRef], interactionId: InteractionId, body: String)
        case replyToInformation(attachments: [AttachmentRef], interactionId: InteractionId, body: String)
        case askAboutPullRequest(attachments: [AttachmentRef], prId: PrId, head: String, body: String)
        case requestConflictResolution(prId: PrId, head: String)
        case messageAgent(attachments: [AttachmentRef], agentId: AgentId, body: String)
        case closeAgent(agentId: AgentId, force: Bool)
        case retryOrchestratorRun(runId: String)
        case cancelOrchestratorRun(runId: String)
        case acknowledgeOrchestratorRun(runId: String)
        /// A kind this build does not know; it decodes so a read that carries one still succeeds.
        case unknown(kind: String)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case requestId = "request_id"
            case body = "body"
            case agentId = "agent_id"
            case name = "name"
            case connected = "connected"
            case generation = "generation"
            case taskId = "task_id"
            case title = "title"
            case reviewPrs = "review_prs"
            case dependencies = "dependencies"
            case stack = "stack"
            case attachments = "attachments"
            case interactionId = "interaction_id"
            case scope = "scope"
            case recipient = "recipient"
            case blocking = "blocking"
            case options = "options"
            case optionId = "option_id"
            case inputs = "inputs"
            case documentId = "document_id"
            case revisionId = "revision_id"
            case contentDigest = "content_digest"
            case contentReference = "content_reference"
            case role = "role"
            case accepted = "accepted"
            case prId = "pr_id"
            case host = "host"
            case repository = "repository"
            case number = "number"
            case head = "head"
            case base = "base"
            case allowedMethods = "allowed_methods"
            case subject = "subject"
            case targets = "targets"
            case isFinal = "is_final"
            case presentationId = "presentation_id"
            case authorizationId = "authorization_id"
            case expectedHead = "expected_head"
            case method = "method"
            case remoteHead = "remote_head"
            case remoteBase = "remote_base"
            case baseVerified = "base_verified"
            case state = "state"
            case mergedHead = "merged_head"
            case ready = "ready"
            case archived = "archived"
            case project = "project"
            case issue = "issue"
            case force = "force"
            case runId = "run_id"
            case answer = "answer"
            case withdrawn = "withdrawn"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "create_request":
                if [.project, .options, .issue, .attachments].contains(where: container.contains) {
                    self = .createRequestWithContext(requestId: try container.decode(RequestId.self, forKey: .requestId), body: try container.decode(String.self, forKey: .body), project: try container.decodeIfPresent(String.self, forKey: .project), options: try container.decodeIfPresent([String: OptionChoice].self, forKey: .options) ?? [:], issue: try container.decodeIfPresent(String.self, forKey: .issue), attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [])
                } else {
                    self = .createRequest(requestId: try container.decode(RequestId.self, forKey: .requestId), body: try container.decode(String.self, forKey: .body))
                }
            case "close_request": self = .closeRequest(requestId: try container.decode(RequestId.self, forKey: .requestId), answer: try container.decodeIfPresent(String.self, forKey: .answer), withdrawn: try container.decodeIfPresent(Bool.self, forKey: .withdrawn) ?? false, title: try container.decodeIfPresent(String.self, forKey: .title))
            case "retitle_task": self = .retitleTask(taskId: try container.decode(TaskId.self, forKey: .taskId), title: try container.decode(String.self, forKey: .title))
            case "withdraw_question": self = .withdrawQuestion(interactionId: try container.decode(InteractionId.self, forKey: .interactionId))
            case "register_agent": self = .registerAgent(agentId: try container.decode(AgentId.self, forKey: .agentId), name: try container.decode(String.self, forKey: .name))
            case "observe_agent": self = .observeAgent(agentId: try container.decode(AgentId.self, forKey: .agentId), connected: try container.decode(Bool.self, forKey: .connected))
            case "advance_orchestrator_generation": self = .advanceOrchestratorGeneration(generation: try container.decode(UInt64.self, forKey: .generation))
            case "create_task": self = .createTask(taskId: try container.decode(TaskId.self, forKey: .taskId), requestId: try container.decode(RequestId.self, forKey: .requestId), title: try container.decode(String.self, forKey: .title), agentId: try container.decodeIfPresent(AgentId.self, forKey: .agentId), reviewPrs: try container.decode(Bool.self, forKey: .reviewPrs), dependencies: try container.decode([TaskId].self, forKey: .dependencies), stack: try container.decodeIfPresent(StackDependency.self, forKey: .stack))
            case "assign_task": self = .assignTask(taskId: try container.decode(TaskId.self, forKey: .taskId), agentId: try container.decode(AgentId.self, forKey: .agentId))
            case "set_dependencies": self = .setDependencies(taskId: try container.decode(TaskId.self, forKey: .taskId), dependencies: try container.decode([TaskId].self, forKey: .dependencies), stack: try container.decodeIfPresent(StackDependency.self, forKey: .stack))
            case "start_task": self = .startTask(taskId: try container.decode(TaskId.self, forKey: .taskId))
            case "publish_information": self = .publishInformation(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], interactionId: try container.decode(InteractionId.self, forKey: .interactionId), scope: try container.decode(Scope.self, forKey: .scope), recipient: try container.decode(Recipient.self, forKey: .recipient), body: try container.decode(String.self, forKey: .body))
            case "ask_question": self = .askQuestion(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], interactionId: try container.decode(InteractionId.self, forKey: .interactionId), scope: try container.decode(Scope.self, forKey: .scope), recipient: try container.decode(Recipient.self, forKey: .recipient), body: try container.decode(String.self, forKey: .body), blocking: try container.decode(Bool.self, forKey: .blocking), options: try container.decode([QuestionOption].self, forKey: .options))
            case "request_action": self = .requestAction(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], interactionId: try container.decode(InteractionId.self, forKey: .interactionId), scope: try container.decode(Scope.self, forKey: .scope), recipient: try container.decode(Recipient.self, forKey: .recipient), body: try container.decode(String.self, forKey: .body), blocking: try container.decode(Bool.self, forKey: .blocking), options: try container.decode([QuestionOption].self, forKey: .options))
            case "mark_read": self = .markRead(interactionId: try container.decode(InteractionId.self, forKey: .interactionId))
            case "answer_question": self = .answerQuestion(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], interactionId: try container.decode(InteractionId.self, forKey: .interactionId), optionId: try container.decode(String.self, forKey: .optionId), inputs: try container.decode([String: InputValue].self, forKey: .inputs))
            case "create_document": self = .createDocument(taskId: try container.decode(TaskId.self, forKey: .taskId), documentId: try container.decode(DocumentId.self, forKey: .documentId), title: try container.decode(String.self, forKey: .title), revisionId: try container.decode(DocumentRevisionId.self, forKey: .revisionId), contentDigest: try container.decode(String.self, forKey: .contentDigest), contentReference: try container.decode(String.self, forKey: .contentReference), role: try container.decode(String.self, forKey: .role))
            case "revise_document": self = .reviseDocument(taskId: try container.decode(TaskId.self, forKey: .taskId), documentId: try container.decode(DocumentId.self, forKey: .documentId), revisionId: try container.decode(DocumentRevisionId.self, forKey: .revisionId), contentDigest: try container.decode(String.self, forKey: .contentDigest), contentReference: try container.decode(String.self, forKey: .contentReference))
            case "link_task_document": self = .linkTaskDocument(taskId: try container.decode(TaskId.self, forKey: .taskId), documentId: try container.decode(DocumentId.self, forKey: .documentId), role: try container.decode(String.self, forKey: .role))
            case "decide_document_review": self = .decideDocumentReview(taskId: try container.decode(TaskId.self, forKey: .taskId), documentId: try container.decode(DocumentId.self, forKey: .documentId), revisionId: try container.decode(DocumentRevisionId.self, forKey: .revisionId), accepted: try container.decode(Bool.self, forKey: .accepted))
            case "register_pr": self = .registerPr(taskId: try container.decode(TaskId.self, forKey: .taskId), prId: try container.decode(PrId.self, forKey: .prId), host: try container.decode(String.self, forKey: .host), repository: try container.decode(String.self, forKey: .repository), number: try container.decode(UInt64.self, forKey: .number), head: try container.decode(String.self, forKey: .head), base: try container.decode(String.self, forKey: .base), allowedMethods: try container.decode([MergeMethod].self, forKey: .allowedMethods))
            case "link_interaction_subject": self = .linkInteractionSubject(interactionId: try container.decode(InteractionId.self, forKey: .interactionId), subject: try container.decode(InteractionSubject.self, forKey: .subject))
            case "present_result": self = .presentResult(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], taskId: try container.decode(TaskId.self, forKey: .taskId), interactionId: try container.decode(InteractionId.self, forKey: .interactionId), body: try container.decode(String.self, forKey: .body), targets: try container.decode([DeliveryTarget].self, forKey: .targets), isFinal: try container.decode(Bool.self, forKey: .isFinal))
            case "submit_feedback": self = .submitFeedback(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], presentationId: try container.decode(InteractionId.self, forKey: .presentationId), targets: try container.decode([DeliveryTarget].self, forKey: .targets), body: try container.decode(String.self, forKey: .body))
            case "accept_delivery": self = .acceptDelivery(presentationId: try container.decode(InteractionId.self, forKey: .presentationId), targets: try container.decode([DeliveryTarget].self, forKey: .targets))
            case "authorize_merge": self = .authorizeMerge(authorizationId: try container.decode(AuthorizationId.self, forKey: .authorizationId), presentationId: try container.decode(InteractionId.self, forKey: .presentationId), prId: try container.decode(PrId.self, forKey: .prId), expectedHead: try container.decode(String.self, forKey: .expectedHead), method: try container.decode(MergeMethod.self, forKey: .method))
            case "claim_merge_authorization": self = .claimMergeAuthorization(authorizationId: try container.decode(AuthorizationId.self, forKey: .authorizationId), remoteHead: try container.decode(String.self, forKey: .remoteHead), remoteBase: try container.decode(String.self, forKey: .remoteBase), baseVerified: try container.decode(Bool.self, forKey: .baseVerified))
            case "observe_merge_attempt": self = .observeMergeAttempt(authorizationId: try container.decode(AuthorizationId.self, forKey: .authorizationId), state: try container.decode(MergeAttemptState.self, forKey: .state))
            case "observe_pr_state": self = .observePrState(prId: try container.decode(PrId.self, forKey: .prId), head: try container.decode(String.self, forKey: .head), base: try container.decode(String.self, forKey: .base), state: try container.decode(PrState.self, forKey: .state), mergedHead: try container.decodeIfPresent(String.self, forKey: .mergedHead))
            case "publish_stack_readiness": self = .publishStackReadiness(taskId: try container.decode(TaskId.self, forKey: .taskId), presentationId: try container.decode(InteractionId.self, forKey: .presentationId), prId: try container.decode(PrId.self, forKey: .prId), head: try container.decode(String.self, forKey: .head), ready: try container.decode(Bool.self, forKey: .ready))
            case "complete_task": self = .completeTask(taskId: try container.decode(TaskId.self, forKey: .taskId), presentationId: try container.decode(InteractionId.self, forKey: .presentationId))
            case "cancel_task": self = .cancelTask(taskId: try container.decode(TaskId.self, forKey: .taskId))
            case "reopen_task": self = .reopenTask(taskId: try container.decode(TaskId.self, forKey: .taskId))
            case "archive_task": self = .archiveTask(taskId: try container.decode(TaskId.self, forKey: .taskId), archived: try container.decode(Bool.self, forKey: .archived))
            case "request_more_context": self = .requestMoreContext(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], interactionId: try container.decode(InteractionId.self, forKey: .interactionId), body: try container.decode(String.self, forKey: .body))
            case "reply_to_information": self = .replyToInformation(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], interactionId: try container.decode(InteractionId.self, forKey: .interactionId), body: try container.decode(String.self, forKey: .body))
            case "ask_about_pull_request": self = .askAboutPullRequest(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], prId: try container.decode(PrId.self, forKey: .prId), head: try container.decode(String.self, forKey: .head), body: try container.decode(String.self, forKey: .body))
            case "request_conflict_resolution": self = .requestConflictResolution(prId: try container.decode(PrId.self, forKey: .prId), head: try container.decode(String.self, forKey: .head))
            case "message_agent": self = .messageAgent(attachments: try container.decodeIfPresent([AttachmentRef].self, forKey: .attachments) ?? [], agentId: try container.decode(AgentId.self, forKey: .agentId), body: try container.decode(String.self, forKey: .body))
            case "close_agent": self = .closeAgent(agentId: try container.decode(AgentId.self, forKey: .agentId), force: try container.decode(Bool.self, forKey: .force))
            case "retry_orchestrator_run": self = .retryOrchestratorRun(runId: try container.decode(String.self, forKey: .runId))
            case "cancel_orchestrator_run": self = .cancelOrchestratorRun(runId: try container.decode(String.self, forKey: .runId))
            case "acknowledge_orchestrator_run": self = .acknowledgeOrchestratorRun(runId: try container.decode(String.self, forKey: .runId))
            case let kind: self = .unknown(kind: kind)
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .createRequest(requestId, body):
                try container.encode("create_request", forKey: .kind)
                try container.encode(requestId, forKey: .requestId)
                try container.encode(body, forKey: .body)
            case let .closeRequest(requestId, answer, withdrawn, title):
                try container.encode("close_request", forKey: .kind)
                try container.encode(requestId, forKey: .requestId)
                try container.encodeIfPresent(answer, forKey: .answer)
                if withdrawn { try container.encode(true, forKey: .withdrawn) }
                try container.encodeIfPresent(title, forKey: .title)
            case let .retitleTask(taskId, title):
                try container.encode("retitle_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(title, forKey: .title)
            case let .withdrawQuestion(interactionId):
                try container.encode("withdraw_question", forKey: .kind)
                try container.encode(interactionId, forKey: .interactionId)
            case let .unknown(kind):
                throw EncodingError.invalidValue(kind, .init(codingPath: encoder.codingPath, debugDescription: "Unknown domain v2 Command kind \(kind) cannot be sent"))
            case let .registerAgent(agentId, name):
                try container.encode("register_agent", forKey: .kind)
                try container.encode(agentId, forKey: .agentId)
                try container.encode(name, forKey: .name)
            case let .observeAgent(agentId, connected):
                try container.encode("observe_agent", forKey: .kind)
                try container.encode(agentId, forKey: .agentId)
                try container.encode(connected, forKey: .connected)
            case let .advanceOrchestratorGeneration(generation):
                try container.encode("advance_orchestrator_generation", forKey: .kind)
                try container.encode(generation, forKey: .generation)
            case let .createTask(taskId, requestId, title, agentId, reviewPrs, dependencies, stack):
                try container.encode("create_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(requestId, forKey: .requestId)
                try container.encode(title, forKey: .title)
                try container.encodeIfPresent(agentId, forKey: .agentId)
                try container.encode(reviewPrs, forKey: .reviewPrs)
                try container.encode(dependencies, forKey: .dependencies)
                try container.encodeIfPresent(stack, forKey: .stack)
            case let .assignTask(taskId, agentId):
                try container.encode("assign_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(agentId, forKey: .agentId)
            case let .setDependencies(taskId, dependencies, stack):
                try container.encode("set_dependencies", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(dependencies, forKey: .dependencies)
                try container.encodeIfPresent(stack, forKey: .stack)
            case let .startTask(taskId):
                try container.encode("start_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
            case let .publishInformation(attachments, interactionId, scope, recipient, body):
                try container.encode("publish_information", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(scope, forKey: .scope)
                try container.encode(recipient, forKey: .recipient)
                try container.encode(body, forKey: .body)
            case let .askQuestion(attachments, interactionId, scope, recipient, body, blocking, options):
                try container.encode("ask_question", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(scope, forKey: .scope)
                try container.encode(recipient, forKey: .recipient)
                try container.encode(body, forKey: .body)
                try container.encode(blocking, forKey: .blocking)
                try container.encode(options, forKey: .options)
            case let .requestAction(attachments, interactionId, scope, recipient, body, blocking, options):
                try container.encode("request_action", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(scope, forKey: .scope)
                try container.encode(recipient, forKey: .recipient)
                try container.encode(body, forKey: .body)
                try container.encode(blocking, forKey: .blocking)
                try container.encode(options, forKey: .options)
            case let .markRead(interactionId):
                try container.encode("mark_read", forKey: .kind)
                try container.encode(interactionId, forKey: .interactionId)
            case let .answerQuestion(attachments, interactionId, optionId, inputs):
                try container.encode("answer_question", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(optionId, forKey: .optionId)
                try container.encode(inputs, forKey: .inputs)
            case let .createDocument(taskId, documentId, title, revisionId, contentDigest, contentReference, role):
                try container.encode("create_document", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(documentId, forKey: .documentId)
                try container.encode(title, forKey: .title)
                try container.encode(revisionId, forKey: .revisionId)
                try container.encode(contentDigest, forKey: .contentDigest)
                try container.encode(contentReference, forKey: .contentReference)
                try container.encode(role, forKey: .role)
            case let .reviseDocument(taskId, documentId, revisionId, contentDigest, contentReference):
                try container.encode("revise_document", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(documentId, forKey: .documentId)
                try container.encode(revisionId, forKey: .revisionId)
                try container.encode(contentDigest, forKey: .contentDigest)
                try container.encode(contentReference, forKey: .contentReference)
            case let .linkTaskDocument(taskId, documentId, role):
                try container.encode("link_task_document", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(documentId, forKey: .documentId)
                try container.encode(role, forKey: .role)
            case let .decideDocumentReview(taskId, documentId, revisionId, accepted):
                try container.encode("decide_document_review", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(documentId, forKey: .documentId)
                try container.encode(revisionId, forKey: .revisionId)
                try container.encode(accepted, forKey: .accepted)
            case let .registerPr(taskId, prId, host, repository, number, head, base, allowedMethods):
                try container.encode("register_pr", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(prId, forKey: .prId)
                try container.encode(host, forKey: .host)
                try container.encode(repository, forKey: .repository)
                try container.encode(number, forKey: .number)
                try container.encode(head, forKey: .head)
                try container.encode(base, forKey: .base)
                try container.encode(allowedMethods, forKey: .allowedMethods)
            case let .linkInteractionSubject(interactionId, subject):
                try container.encode("link_interaction_subject", forKey: .kind)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(subject, forKey: .subject)
            case let .presentResult(attachments, taskId, interactionId, body, targets, isFinal):
                try container.encode("present_result", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(body, forKey: .body)
                try container.encode(targets, forKey: .targets)
                try container.encode(isFinal, forKey: .isFinal)
            case let .submitFeedback(attachments, presentationId, targets, body):
                try container.encode("submit_feedback", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(presentationId, forKey: .presentationId)
                try container.encode(targets, forKey: .targets)
                try container.encode(body, forKey: .body)
            case let .acceptDelivery(presentationId, targets):
                try container.encode("accept_delivery", forKey: .kind)
                try container.encode(presentationId, forKey: .presentationId)
                try container.encode(targets, forKey: .targets)
            case let .authorizeMerge(authorizationId, presentationId, prId, expectedHead, method):
                try container.encode("authorize_merge", forKey: .kind)
                try container.encode(authorizationId, forKey: .authorizationId)
                try container.encode(presentationId, forKey: .presentationId)
                try container.encode(prId, forKey: .prId)
                try container.encode(expectedHead, forKey: .expectedHead)
                try container.encode(method, forKey: .method)
            case let .claimMergeAuthorization(authorizationId, remoteHead, remoteBase, baseVerified):
                try container.encode("claim_merge_authorization", forKey: .kind)
                try container.encode(authorizationId, forKey: .authorizationId)
                try container.encode(remoteHead, forKey: .remoteHead)
                try container.encode(remoteBase, forKey: .remoteBase)
                try container.encode(baseVerified, forKey: .baseVerified)
            case let .observeMergeAttempt(authorizationId, state):
                try container.encode("observe_merge_attempt", forKey: .kind)
                try container.encode(authorizationId, forKey: .authorizationId)
                try container.encode(state, forKey: .state)
            case let .observePrState(prId, head, base, state, mergedHead):
                try container.encode("observe_pr_state", forKey: .kind)
                try container.encode(prId, forKey: .prId)
                try container.encode(head, forKey: .head)
                try container.encode(base, forKey: .base)
                try container.encode(state, forKey: .state)
                try container.encodeIfPresent(mergedHead, forKey: .mergedHead)
            case let .publishStackReadiness(taskId, presentationId, prId, head, ready):
                try container.encode("publish_stack_readiness", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(presentationId, forKey: .presentationId)
                try container.encode(prId, forKey: .prId)
                try container.encode(head, forKey: .head)
                try container.encode(ready, forKey: .ready)
            case let .completeTask(taskId, presentationId):
                try container.encode("complete_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(presentationId, forKey: .presentationId)
            case let .cancelTask(taskId):
                try container.encode("cancel_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
            case let .reopenTask(taskId):
                try container.encode("reopen_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
            case let .archiveTask(taskId, archived):
                try container.encode("archive_task", forKey: .kind)
                try container.encode(taskId, forKey: .taskId)
                try container.encode(archived, forKey: .archived)
            case let .createRequestWithContext(requestId, body, project, options, issue, attachments):
                try container.encode("create_request", forKey: .kind)
                try container.encode(requestId, forKey: .requestId)
                try container.encode(body, forKey: .body)
                try container.encodeIfPresent(project, forKey: .project)
                try container.encode(options, forKey: .options)
                try container.encodeIfPresent(issue, forKey: .issue)
                try container.encode(attachments, forKey: .attachments)
            case let .requestMoreContext(attachments, interactionId, body):
                try container.encode("request_more_context", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(body, forKey: .body)
            case let .replyToInformation(attachments, interactionId, body):
                try container.encode("reply_to_information", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(interactionId, forKey: .interactionId)
                try container.encode(body, forKey: .body)
            case let .askAboutPullRequest(attachments, prId, head, body):
                try container.encode("ask_about_pull_request", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(prId, forKey: .prId)
                try container.encode(head, forKey: .head)
                try container.encode(body, forKey: .body)
            case let .requestConflictResolution(prId, head):
                try container.encode("request_conflict_resolution", forKey: .kind)
                try container.encode(prId, forKey: .prId)
                try container.encode(head, forKey: .head)
            case let .messageAgent(attachments, agentId, body):
                try container.encode("message_agent", forKey: .kind)
                try container.encode(attachments, forKey: .attachments)
                try container.encode(agentId, forKey: .agentId)
                try container.encode(body, forKey: .body)
            case let .closeAgent(agentId, force):
                try container.encode("close_agent", forKey: .kind)
                try container.encode(agentId, forKey: .agentId)
                try container.encode(force, forKey: .force)
            case let .retryOrchestratorRun(runId):
                try container.encode("retry_orchestrator_run", forKey: .kind)
                try container.encode(runId, forKey: .runId)
            case let .cancelOrchestratorRun(runId):
                try container.encode("cancel_orchestrator_run", forKey: .kind)
                try container.encode(runId, forKey: .runId)
            case let .acknowledgeOrchestratorRun(runId):
                try container.encode("acknowledge_orchestrator_run", forKey: .kind)
                try container.encode(runId, forKey: .runId)
            }
        }
    }

    public enum DomainEffect: Codable, Equatable, Sendable {
        case notifyUser(taskId: TaskId?, interactionId: InteractionId)
        case notifyAgent(agentId: AgentId, taskId: TaskId?, interactionId: InteractionId?, message: String)
        case notifyOrchestrator(interactionId: InteractionId)

        enum CodingKeys: String, CodingKey {
            case kind = "kind"
            case agentId = "agent_id"
            case taskId = "task_id"
            case interactionId = "interaction_id"
            case message = "message"
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .kind) {
            case "notify_user": self = .notifyUser(taskId: try container.decodeIfPresent(TaskId.self, forKey: .taskId), interactionId: try container.decode(InteractionId.self, forKey: .interactionId))
            case "notify_agent": self = .notifyAgent(agentId: try container.decode(AgentId.self, forKey: .agentId), taskId: try container.decodeIfPresent(TaskId.self, forKey: .taskId), interactionId: try container.decodeIfPresent(InteractionId.self, forKey: .interactionId), message: try container.decode(String.self, forKey: .message))
            case "notify_orchestrator": self = .notifyOrchestrator(interactionId: try container.decode(InteractionId.self, forKey: .interactionId))
            default: throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown domain v2 DomainEffect kind")
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .notifyUser(taskId, interactionId):
                try container.encode("notify_user", forKey: .kind)
                try container.encodeIfPresent(taskId, forKey: .taskId)
                try container.encode(interactionId, forKey: .interactionId)
            case let .notifyAgent(agentId, taskId, interactionId, message):
                try container.encode("notify_agent", forKey: .kind)
                try container.encode(agentId, forKey: .agentId)
                try container.encodeIfPresent(taskId, forKey: .taskId)
                try container.encodeIfPresent(interactionId, forKey: .interactionId)
                try container.encode(message, forKey: .message)
            case let .notifyOrchestrator(interactionId):
                try container.encode("notify_orchestrator", forKey: .kind)
                try container.encode(interactionId, forKey: .interactionId)
            }
        }
    }

}
