import AlondraContracts
import AlondraDomain
import AlondraShared
import ComposableArchitecture
import CryptoKit
import Foundation

/// A hand-built v2 projection, grouped the way the Mac serves it: questions and information are
/// roots, a PR is a root with its presentation and questions as children, and a document review
/// is a root with its delivery as child.
struct V2Fixture {
    static let epoch: DomainV2.DataEpoch = "epoch"
    static let identity = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "phone",
                                         credentialRevision: "one", generation: 1)
    static let other = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "other",
                                      credentialRevision: "two", generation: 2)
    static let workIdentity = QuestionWorkIdentity(server: identity.server, deviceID: identity.deviceID,
                                                   credentialRevision: identity.credentialRevision, generation: identity.generation)
    static let agent: DomainV2.AgentId = "agent-1"
    static let request: DomainV2.RequestId = "request-1"
    static let task: DomainV2.TaskId = "t1"
    static let head = "head"
    /// The option an answer written in words goes to.
    static let reply = DomainV2.QuestionOption(id: "reply", label: "Reply", inputs: [.init(id: "text", kind: .text, required: false)])

    var revision: DomainV2.Revision = 1
    var inbox: [DomainV2.InboxItem] = []
    var tasks: [DomainV2.Task] = []
    var agents: [DomainV2.Agent] = [.init(id: agent, name: "Node", connected: true)]
    var requests: [DomainV2.Request] = [.init(id: request, body: "Build it", closed: false)]
    var interactions: [DomainV2.Interaction] = []
    var pullRequests: [DomainV2.PullRequest] = []
    var documents: [DomainV2.Document] = []
    var documentRevisions: [DomainV2.DocumentRevision] = []
    var runtime: DomainV2.Runtime?

    init() {
        tasks = [Self.makeTask(Self.task)]
    }

    static func makeTask(_ id: DomainV2.TaskId, request: DomainV2.RequestId = request, state: DomainV2.TaskState = .running,
                         prId: DomainV2.PrId? = nil, archived: Bool = false) -> DomainV2.Task {
        .init(id: id, requestId: request, agentId: agent, title: "Task \(id.rawValue)", state: state, revision: 1, started: true,
              reviewPrs: true, finalPresentation: nil, prId: prId, dependencies: [], stack: nil, documents: [:], archived: archived)
    }

    func action(_ kind: DomainV2.ActionKind, presentation: DomainV2.InteractionId? = nil, head: String? = nil) -> DomainV2.AvailableAction {
        .init(kind: kind.rawValue, enabled: true, disabledReason: nil, expectedRevision: revision, presentationId: presentation, expectedHead: head)
    }

    private mutating func interaction(_ id: String, body: String, payload: DomainV2.InteractionPayload,
                                      attachments: [DomainV2.AttachmentRef] = []) -> DomainV2.InteractionId {
        let interactionID = DomainV2.InteractionId(rawValue: id)
        interactions.append(.init(attachments: attachments, id: interactionID, scope: .task(taskId: Self.task), author: .agent(agentId: Self.agent),
                                  recipient: .user, body: body, blocking: false, subject: nil, payload: payload, revision: revision,
                                  order: UInt64(interactions.count + 1)))
        return interactionID
    }

    private func entry(_ id: DomainV2.InteractionId, kind: String, title: String, body: String, order: UInt64,
                       actions: [DomainV2.AvailableAction]) -> DomainV2.InboxItem {
        .init(id: "interaction:\(id.rawValue)", kind: kind, subject: .interaction(interactionId: id), title: title, body: body,
              taskId: Self.task, requestId: Self.request, agentId: Self.agent, revision: revision, order: order, pending: true,
              children: [], availableActions: actions)
    }

    mutating func questionItem(_ id: String, title: String = "Proceed?", options: [DomainV2.QuestionOption] = [reply],
                               order: UInt64? = nil) -> DomainV2.InboxItem {
        let interaction = interaction(id, body: "Which one?", payload: .question(options: options, answer: nil))
        return entry(interaction, kind: "question", title: title, body: "Which one?", order: order ?? UInt64(interactions.count),
                     actions: [action(.answerQuestion)])
    }

    @discardableResult
    mutating func question(_ id: String, title: String = "Proceed?", options: [DomainV2.QuestionOption] = [reply],
                           order: UInt64? = nil) -> DomainV2.InboxItem {
        let item = questionItem(id, title: title, options: options, order: order)
        inbox.append(item)
        return item
    }

    @discardableResult
    mutating func information(_ id: String, title: String = "Report", body: String = "Body", order: UInt64? = nil) -> DomainV2.InboxItem {
        let interaction = interaction(id, body: body, payload: .information(readBy: []))
        let item = entry(interaction, kind: "information", title: title, body: body, order: order ?? UInt64(interactions.count),
                         actions: [action(.markRead)])
        inbox.append(item)
        return item
    }

    /// A PR root whose presentation offers merge and feedback, with `questions` asked about it.
    @discardableResult
    mutating func pullRequest(_ number: UInt64, questions: [String] = [], order: UInt64? = nil) -> DomainV2.InboxItem {
        let prID = DomainV2.PrId(rawValue: "pr-\(number)")
        pullRequests.append(.init(id: prID, taskId: Self.task, host: "github.com", repository: "a/b", number: number, head: Self.head,
                                  base: "main", state: .open, mergedHead: nil, allowedMethods: [.merge, .squash], review: nil))
        let presentation = interaction("delivery-\(number)", body: "PR \(number) is ready",
                                       payload: .delivery(targets: [.pullRequest(prId: prID, head: Self.head)], isFinal: true, acceptance: nil, feedback: []))
        let rootOrder = order ?? UInt64(interactions.count)
        var children = [entry(presentation, kind: "delivery", title: "PR \(number)", body: "PR \(number) is ready", order: rootOrder,
                              actions: [action(.acceptDelivery, presentation: presentation), action(.submitFeedback, presentation: presentation),
                                        action(.authorizeMerge, presentation: presentation, head: Self.head)])]
        for question in questions { children.append(questionItem(question, order: rootOrder)) }
        let root = DomainV2.InboxItem(id: "pr:\(prID.rawValue)", kind: "pull_request", subject: .pullRequest(prId: prID), title: "a/b #\(number)",
                                      body: nil, taskId: Self.task, requestId: Self.request, agentId: Self.agent, revision: revision,
                                      order: rootOrder, pending: true, children: children, availableActions: [])
        inbox.append(root)
        return root
    }

    /// A document review root whose pending delivery offers accept and feedback.
    @discardableResult
    mutating func documentReview(_ name: String, text: String = "Body", order: UInt64? = nil) -> DomainV2.InboxItem {
        let documentID = DomainV2.DocumentId(rawValue: "doc-\(name)")
        let revisionID = DomainV2.DocumentRevisionId(rawValue: "rev-\(name)")
        documents.append(.init(id: documentID, title: "Document \(name)", currentRevision: revisionID))
        documentRevisions.append(.init(id: revisionID, documentId: documentID, number: 1, contentDigest: Self.digest(text),
                                       contentReference: "ref-\(name)"))
        let delivery = interaction("delivery-\(name)", body: "Please review",
                                   payload: .delivery(targets: [.documentRevision(documentId: documentID, revisionId: revisionID)],
                                                      isFinal: true, acceptance: nil, feedback: []))
        let rootOrder = order ?? UInt64(interactions.count)
        let child = entry(delivery, kind: "delivery", title: "Document \(name)", body: "Please review", order: rootOrder,
                          actions: [action(.acceptDelivery, presentation: delivery), action(.submitFeedback, presentation: delivery)])
        let root = DomainV2.InboxItem(id: "document:\(Self.task.rawValue):\(documentID.rawValue):\(revisionID.rawValue)", kind: "document_review",
                                      subject: .documentReview(taskId: Self.task, documentId: documentID, revisionId: revisionID),
                                      title: "Document \(name)", body: nil, taskId: Self.task, requestId: Self.request, agentId: Self.agent,
                                      revision: revision, order: rootOrder, pending: true, children: [child], availableActions: [])
        inbox.append(root)
        return root
    }

    /// Runtime telemetry as the Mac serves it; `body` holds the fields besides the envelope.
    static func runtime(_ body: String = #""nodes":[]"#) -> DomainV2.Runtime {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let json = #"{"protocol_version":2,"data_epoch":"\#(epoch.rawValue)",\#(body)}"#
        return try! decoder.decode(DomainV2.Runtime.self, from: Data(json.utf8))
    }

    static func digest(_ text: String) -> String {
        Data(text.utf8).sha256Hex
    }

    var workspace: DomainWorkspace {
        let projection = DomainV2.DomainProjection(
            protocolVersion: DomainV2.protocolVersion, dataEpoch: Self.epoch, revision: revision, inbox: inbox,
            counts: .init(roots: UInt64(inbox.count), pending: UInt64(inbox.filter(\.pending).count), unreadInformation: 0),
            tasks: tasks, agents: agents, requests: requests, interactions: interactions, pullRequests: pullRequests,
            documents: documents, documentRevisions: documentRevisions, documentReviews: [], mergeAuthorizations: [], stackReadiness: [])
        return DomainWorkspace(projection, runtime: runtime)
    }

    func update(_ identity: TriageIdentity = identity, connection: TriageConnection = .online) -> DomainWorkspaceUpdate {
        DomainWorkspaceUpdate(identity: identity, workspace: workspace, connection: connection)
    }

    static let recorded = DomainCommandResult.recorded(.init(revision: 2, changed: true, effects: [], createdIds: []))
    static func rejected(_ message: String = "Rejected") -> DomainCommandResult {
        .rejected(DomainCommandRejection(code: "invalid_state", message: message))
    }
}

extension Data {
    var sha256Hex: String { SHA256.hash(data: self).map { String(format: "%02x", $0) }.joined() }
}

/// Every command a reducer sends, with the command ID it carried.
final class CommandLog: Sendable {
    let sent = LockIsolated<[(intent: DomainIntent, commandID: String)]>([])
    var intents: [DomainIntent] { sent.value.map(\.intent) }
    var commandIDs: [String] { sent.value.map(\.commandID) }
    var count: Int { sent.value.count }
}

extension DomainWorkspaceClient {
    /// A server double: `perform` answers every command, the diff and documents are served whole.
    static func scripted(_ log: CommandLog, diff: String = "", documents: [String: String] = [:],
                         taskActions: [DomainV2.AvailableAction] = [],
                         observe: @escaping @Sendable () async -> AsyncStream<DomainWorkspaceUpdate> = { AsyncStream { $0.finish() } },
                         perform: @escaping @Sendable (DomainIntent) async -> DomainCommandResult) -> DomainWorkspaceClient {
        DomainWorkspaceClient(
            observe: observe,
            perform: { intent, _, _, commandID in
                log.sent.withValue { $0.append((intent, commandID)) }
                return await perform(intent)
            },
            taskActions: { _, _ in taskActions },
            pullRequestDiff: { id, head in
                DomainV2.PullRequestDiff(prId: id, host: "github.com", repository: "a/b", number: 1, head: head, diff: diff, truncated: false)
            },
            documentContent: { id, _ in
                guard let text = documents[id.rawValue] else { throw CancellationError() }
                return text
            })
    }
}

/// A command that waits until the test releases it with a result.
actor CommandGate {
    private var started = false
    private var startWaiters: [CheckedContinuation<Void, Never>] = []
    private var result: DomainCommandResult?
    private var resultWaiter: CheckedContinuation<DomainCommandResult, Never>?

    func run() async -> DomainCommandResult {
        started = true
        for waiter in startWaiters { waiter.resume() }
        startWaiters = []
        if let result { return result }
        return await withCheckedContinuation { resultWaiter = $0 }
    }

    func waitUntilStarted() async {
        if started { return }
        await withCheckedContinuation { startWaiters.append($0) }
    }

    func release(_ value: DomainCommandResult) {
        if let resultWaiter { resultWaiter.resume(returning: value); self.resultWaiter = nil } else { result = value }
    }
}
