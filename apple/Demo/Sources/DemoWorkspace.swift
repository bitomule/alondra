import AlondraAttachmentsData
import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraShared
import AlondraTasksFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import CryptoKit
import Foundation

/// Synthetic session: no network, credentials, persistence, notifications or filesystem I/O.
/// It plays the server: it groups the inbox and offers the actions a real Mac would, and every
/// command changes only this in-memory copy. Closing it invalidates captured dependencies.
public actor DemoWorkspace {
    public nonisolated let identity: TriageIdentity
    private var state: DemoState
    private var active = true
    private var observers: [UUID: AsyncStream<DomainWorkspaceUpdate>.Continuation] = [:]
    private var files: [UUID: (String, Data)] = [:]
    private var schedules: [ScheduledTask] = []
    private var commands: [String: DomainCommandResult] = [:]
    private var nextID: UInt64 = 10

    public init(now: Date = .now) throws {
        state = DemoState.initial(now: Int64(now.timeIntervalSince1970 * 1000))
        identity = TriageIdentity(server: URL(string: "demo://workspace")!, deviceID: "demo",
                                  credentialRevision: UUID().uuidString, generation: 0)
    }

    public func workspace() -> DomainWorkspace? { active ? state.workspace : nil }

    public func close() {
        active = false
        files.removeAll(); schedules.removeAll(); commands.removeAll()
        for continuation in observers.values {
            continuation.yield(DomainWorkspaceUpdate(identity: identity, workspace: nil, connection: .connecting))
            continuation.finish()
        }
        observers.removeAll()
    }

    public func updates() -> AsyncStream<DomainWorkspaceUpdate> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<DomainWorkspaceUpdate>.makeStream(bufferingPolicy: .bufferingNewest(1))
        guard active else { continuation.finish(); return stream }
        observers[id] = continuation
        continuation.yield(DomainWorkspaceUpdate(identity: identity, workspace: state.workspace, connection: .online))
        continuation.onTermination = { [weak self] _ in Task { await self?.removeObserver(id) } }
        return stream
    }

    private func removeObserver(_ id: UUID) { observers.removeValue(forKey: id) }

    private func publish() {
        state.revision += 1
        let workspace = state.workspace
        for continuation in observers.values {
            continuation.yield(DomainWorkspaceUpdate(identity: identity, workspace: workspace, connection: .online))
        }
    }

    private func ensureActive() throws { guard active else { throw CancellationError() } }

    public func storeAttachment(id: UUID, name: String, data: Data) throws -> DomainV2.AttachmentRef {
        try ensureActive()
        if let existing = files[id], existing.0 != name || existing.1 != data { throw DemoError.attachmentConflict }
        files[id] = (name, data)
        let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        return DomainV2.AttachmentRef(id: "demo-attachment-" + id.uuidString.lowercased(), name: name,
                                      contentDigest: digest, contentReference: "demo://attachments/" + id.uuidString, bytes: UInt64(data.count))
    }

    /// A retry with the same command ID returns the first result, as the Mac does.
    private func perform(_ intent: DomainIntent, commandID: String) -> DomainCommandResult {
        guard active else { return .obsolete }
        if let first = commands[commandID] { return first }
        let result: DomainCommandResult
        if let message = state.apply(intent, nextID: &nextID) {
            result = .rejected(DomainCommandRejection(code: "invalid_state", message: message))
        } else {
            publish()
            result = .recorded(DomainV2.CommandOutcome(revision: state.revision, changed: true, effects: [], createdIds: []))
        }
        commands[commandID] = result
        return result
    }

    private func create(_ body: NewRequestBody, commandID: String) -> CreateRequestResult {
        guard active else { return .cancelled }
        let known = Set(files.keys.map { "demo-attachment-" + $0.uuidString.lowercased() })
        guard body.attachments.allSatisfy({ known.contains($0.id) }) else { return .failed("The demo attachment is no longer available") }
        switch perform(.createRequest(body: body.text, project: body.project, options: body.options, issue: body.issue,
                                      attachments: body.attachments), commandID: commandID) {
        case .recorded: return .confirmed
        case .rejected(let rejection): return .failed(rejection.message)
        case .uncertain, .obsolete: return .cancelled
        }
    }

    private func node(_ id: String, message: NodeMessage?, commandID: String) -> NodeMutationOutcome {
        let agent = DomainV2.AgentId(rawValue: id)
        let refs: [DomainV2.AttachmentRef]
        do { refs = try (message?.files ?? []).map { try storeAttachment(id: $0.id, name: $0.name, data: $0.data) } }
        catch { return .rejected(error.localizedDescription) }
        let intent: DomainIntent = message.map { .messageAgent(agent, body: $0.text, attachments: refs) } ?? .closeAgent(agent, force: false)
        switch perform(intent, commandID: commandID) {
        case .recorded: return .confirmed
        case .rejected(let rejection): return .rejected(rejection.message)
        case .uncertain: return .unknown
        case .obsolete: return .obsolete
        }
    }

    private func scheduleList() throws -> [ScheduledTask] { try ensureActive(); return schedules }

    private func saveSchedule(_ draft: ScheduledTaskDraft, id: String?) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        guard let wire = DomainV2.ScheduleDraft(draft) else { return .rejected("The demo schedule is incomplete") }
        let scheduleID = id ?? "demo-schedule-\(nextID)"
        nextID += 1
        do {
            guard var object = try JSONSerialization.jsonObject(with: JSONEncoder().encode(wire)) as? [String: Any] else {
                return .rejected("The demo schedule is incomplete")
            }
            object["id"] = scheduleID
            object["status"] = DomainV2.ScheduleStatus.neverRun.rawValue
            let schedule = try DomainV2.decode(DomainV2.Schedule.self, from: JSONSerialization.data(withJSONObject: object))
            schedules.removeAll { $0.id == scheduleID }
            schedules.append(ScheduledTask(schedule))
            return .confirmed
        } catch { return .rejected(error.localizedDescription) }
    }

    private func runSchedule(_ id: String, commandID: String) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        guard let schedule = schedules.first(where: { $0.id == id }) else { return .rejected("Demo schedule not found") }
        switch create(NewRequestBody(text: schedule.text, project: nil, options: [:], issue: nil), commandID: commandID) {
        case .confirmed: return .confirmed
        default: return .rejected("Could not create the demo task")
        }
    }

    private func deleteSchedule(_ id: String) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        schedules.removeAll { $0.id == id }
        return .confirmed
    }

    private func options(project: String?) throws -> OptionsCatalog { try ensureActive(); return try DemoFixture.decode(DemoFixture.options(project: project)) }
    private func issues() throws -> IssueList { try ensureActive(); return try DemoFixture.decode(["issues": []]) }

    private func scope() -> NewWorkSubmissionScope? {
        guard active else { return nil }
        return NewWorkSubmissionScope(upload: { try await self.storeAttachment(id: $0.id, name: $0.name, data: $0.data) },
            isCurrent: { await self.active }, create: { await self.create($0, commandID: $1) })
    }

    private func document(_ id: DomainV2.DocumentRevisionId, digest: String) throws -> String {
        try ensureActive()
        guard id == DemoState.revisionID, digest == DemoFixture.documentDigest else { throw DemoError.missingItem }
        return DemoFixture.document
    }

    private func diff(_ id: DomainV2.PrId, head: String) throws -> DomainV2.PullRequestDiff {
        try ensureActive()
        guard let pr = state.pullRequests.first(where: { $0.id == id }), pr.head == head else { throw DemoError.missingItem }
        return DomainV2.PullRequestDiff(prId: id, host: pr.host, repository: pr.repository, number: pr.number,
                                        head: pr.head, diff: DemoFixture.diff, truncated: false)
    }

    private func taskActions(_ id: DomainV2.TaskId) throws -> [DomainV2.AvailableAction] {
        try ensureActive()
        return state.taskActions(id)
    }

    public nonisolated func install(into values: inout DependencyValues) {
        values.attachmentImport = AttachmentImportClient(file: { try AttachmentImporter.file($0) }, photo: { try await AttachmentImporter.photo($0) }, camera: { try AttachmentImporter.camera($0) }, thumbnail: { await AttachmentImporter.thumbnail($0) })
        values.attachmentUpload = AttachmentUploadClient(upload: {
            try await self.storeAttachment(id: $0.id, name: $0.name, data: $0.data)
        })
        values.domainWorkspace = DomainWorkspaceClient(
            observe: { await self.updates() },
            perform: { intent, _, _, commandID in await self.perform(intent, commandID: commandID) },
            taskActions: { id, _ in try await self.taskActions(id) },
            pullRequestDiff: { id, head in try await self.diff(id, head: head) },
            documentContent: { id, digest in try await self.document(id, digest: digest) })
        values.serviceTasksClient = ServiceTasksClient(refresh: { await self.publish() })
        values.scheduleOperations = ScheduleOperations(list: { try await self.scheduleList() }, save: { draft, id, _ in await self.saveSchedule(draft, id: id) },
            run: { await self.runSchedule($0, commandID: $1) }, delete: { id, _ in await self.deleteSchedule(id) },
            projects: { ["Demo Garden"] }, options: { try await self.options(project: $0).options })
        values.nodeOperations = NodeOperations(send: { await self.node($0, message: $1, commandID: $2) },
            close: { await self.node($0, message: nil, commandID: $1) }, threads: { _ in [] })
        values.newWorkOperations = NewWorkOperations(projects: { ["Demo Garden"] }, options: { try await self.options(project: $0) },
            issues: { _ in try await self.issues() }, captureSubmissionScope: { await self.scope() })
    }

    private enum DemoError: Error { case missingItem, attachmentConflict }
}

/// The synthetic server state. `workspace` groups it into the inbox a Mac would serve.
struct DemoState {
    static let epoch = DomainV2.DataEpoch(rawValue: "demo")
    static let designAgent = DomainV2.AgentId(rawValue: "demo-design")
    static let request = DomainV2.RequestId(rawValue: "demo-request")
    static let designTask = DomainV2.TaskId(rawValue: "demo-task")
    static let copyTask = DomainV2.TaskId(rawValue: "demo-copy")
    static let pullRequest = DomainV2.PrId(rawValue: "demo-pr")
    static let documentID = DomainV2.DocumentId(rawValue: "demo-document")
    static let revisionID = DomainV2.DocumentRevisionId(rawValue: "demo-document-1")
    static let head = String(repeating: "d", count: 40)

    var revision: DomainV2.Revision = 1
    var tasks: [DomainV2.Task]
    var agents: [DomainV2.Agent]
    var requests: [DomainV2.Request]
    var interactions: [DomainV2.Interaction]
    var pullRequests: [DomainV2.PullRequest]
    var documents: [DomainV2.Document]
    var documentRevisions: [DomainV2.DocumentRevision]
    var runtime: DomainV2.Runtime
    /// Interactions the user has dealt with: they stay in history but leave the inbox.
    var settled: Set<DomainV2.InteractionId> = []

    static func initial(now: Int64) -> DemoState {
        func interaction(_ id: String, task: DomainV2.TaskId, body: String, blocking: Bool = false,
                         subject: DomainV2.InteractionSubject? = nil, payload: DomainV2.InteractionPayload, order: UInt64) -> DomainV2.Interaction {
            DomainV2.Interaction(attachments: [], id: .init(rawValue: id), scope: .task(taskId: task), author: .agent(agentId: designAgent),
                                 recipient: .user, body: body, blocking: blocking, subject: subject, payload: payload, revision: 1, order: order)
        }
        let question = interaction("demo-question", task: designTask,
            body: "Which welcome screen should we use? Choose the calmer layout or keep the current design.", blocking: true,
            payload: .question(options: [.init(id: "calmer", label: "Calmer layout", inputs: []),
                                         .init(id: "keep", label: "Keep current", inputs: []),
                                         .init(id: "other", label: "Something else", inputs: [.init(id: "explanation", kind: .text, required: true)])],
                               answer: nil), order: 1)
        let delivery = interaction("demo-delivery", task: designTask,
            body: "The welcome message is shorter. Every action on this pull request changes only this demo.",
            payload: .delivery(targets: [.pullRequest(prId: pullRequest, head: head)], isFinal: true, acceptance: nil, feedback: []), order: 2)
        let proposal = interaction("demo-proposal", task: copyTask,
            body: "A short plan for the welcome screen. Review the sample document and leave feedback.",
            payload: .delivery(targets: [.documentRevision(documentId: documentID, revisionId: revisionID)], isFinal: true, acceptance: nil, feedback: []), order: 3)
        let update = interaction("demo-update", task: designTask,
            body: "The design assistant finished exploring three layouts.", payload: .information(readBy: []), order: 4)
        let node = try? DemoFixture.decode(["sessionId": designAgent.rawValue, "shortId": "design", "name": "Design assistant",
            "project": "Demo Garden", "isOrchestrator": true, "hasPlugin": true, "state": "waiting_answer", "stateSince": now,
            "stateSource": "demo", "lastMessage": "Waiting for your feedback on the example work."], as: Node.self)
        let machine = try? DemoFixture.decode(["memoryTotal": 16_000_000_000, "memoryAvailable": 10_000_000_000, "load": [0.2, 0.3, 0.2],
            "diskAvailable": 120_000_000_000, "unreliable": false], as: Machine.self)
        return DemoState(
            tasks: [
                .init(id: designTask, requestId: request, agentId: designAgent, title: "Polish the welcome experience", state: .needsYou,
                      revision: 1, started: true, reviewPrs: true, finalPresentation: delivery.id, prId: pullRequest,
                      dependencies: [], stack: nil, documents: [:], archived: false),
                .init(id: copyTask, requestId: request, agentId: designAgent, title: "Write the welcome copy", state: .inReview,
                      revision: 1, started: true, reviewPrs: false, finalPresentation: proposal.id, prId: nil,
                      dependencies: [], stack: nil, documents: [documentID.rawValue: "proposal"], archived: false),
            ],
            agents: [.init(id: designAgent, name: "Design assistant", connected: true)],
            requests: [.init(id: request, body: "Make the welcome experience simpler", closed: false)],
            interactions: [question, delivery, proposal, update],
            pullRequests: [.init(id: pullRequest, taskId: designTask, host: "github.com", repository: "demo/garden", number: 3,
                                 head: head, base: "main", state: .open, mergedHead: nil, allowedMethods: [.squash], review: nil)],
            documents: [.init(id: documentID, title: "Welcome screen proposal", currentRevision: revisionID)],
            documentRevisions: [.init(id: revisionID, documentId: documentID, number: 1, contentDigest: DemoFixture.documentDigest,
                                      contentReference: "demo://welcome.md")],
            runtime: DomainV2.Runtime(machine: machine, nodes: node.map { [$0] } ?? [],
                                      tasks: [designTask.rawValue: .init(project: "Demo Garden", stateSince: now),
                                              copyTask.rawValue: .init(project: "Demo Garden", stateSince: now)],
                                      requests: [request.rawValue: .init(createdAt: now)],
                                      pullRequests: [pullRequest.rawValue: .init(title: "Simplify the welcome message",
                                          body: "Example pull request. Every action changes only this demo.",
                                          checks: [PRCheck(name: "Example integration checks", state: "passed")])]))
    }

    var workspace: DomainWorkspace {
        let inbox = self.inbox
        let projection = DomainV2.DomainProjection(
            protocolVersion: DomainV2.protocolVersion, dataEpoch: Self.epoch, revision: revision, inbox: inbox,
            counts: .init(roots: UInt64(inbox.count), pending: UInt64(inbox.filter(\.pending).count), unreadInformation: 0),
            tasks: tasks, agents: agents, requests: requests, interactions: interactions, pullRequests: pullRequests,
            documents: documents, documentRevisions: documentRevisions, documentReviews: [], mergeAuthorizations: [], stackReadiness: [])
        return DomainWorkspace(projection, runtime: runtime)
    }

    private func action(_ kind: DomainV2.ActionKind, presentation: DomainV2.InteractionId? = nil, head: String? = nil) -> DomainV2.AvailableAction {
        .init(kind: kind.rawValue, enabled: true, disabledReason: nil, expectedRevision: revision, presentationId: presentation, expectedHead: head)
    }

    private func item(_ interaction: DomainV2.Interaction) -> DomainV2.InboxItem {
        let task = tasks.first { if case .task(let id) = interaction.scope { $0.id == id } else { false } }
        let kind: String
        var actions: [DomainV2.AvailableAction]
        switch interaction.payload {
        case .question, .actionRequest: kind = "question"; actions = [action(.answerQuestion)]
        case .information: kind = "information"; actions = [action(.markRead)]
        case .delivery(let targets, _, _, _):
            kind = "delivery"
            actions = [action(.acceptDelivery, presentation: interaction.id), action(.submitFeedback, presentation: interaction.id)]
            for case .pullRequest(_, let head) in targets { actions.append(action(.authorizeMerge, presentation: interaction.id, head: head)) }
        }
        return .init(id: "interaction:\(interaction.id.rawValue)", kind: kind, subject: .interaction(interactionId: interaction.id),
                     title: task?.title ?? "Orchestrator", body: interaction.body, taskId: task?.id, requestId: task?.requestId,
                     agentId: task?.agentId, revision: revision, order: interaction.order, pending: true, children: [],
                     availableActions: actions)
    }

    var inbox: [DomainV2.InboxItem] {
        var roots: [DomainV2.InboxItem] = []
        for interaction in interactions where !settled.contains(interaction.id) {
            let entry = item(interaction)
            guard case .delivery(let targets, _, _, _) = interaction.payload, let task = tasks.first(where: { $0.id == entry.taskId }) else {
                roots.append(entry); continue
            }
            if case .pullRequest(let prID, _) = targets.first, let pr = pullRequests.first(where: { $0.id == prID }) {
                guard pr.state == .open else { continue }
                roots.append(.init(id: "pr:\(prID.rawValue)", kind: "pull_request", subject: .pullRequest(prId: prID),
                                   title: "\(pr.repository) #\(pr.number)", body: nil, taskId: task.id, requestId: task.requestId,
                                   agentId: task.agentId, revision: revision, order: interaction.order, pending: true,
                                   children: [entry], availableActions: []))
            } else if case .documentRevision(let documentID, let revisionID) = targets.first {
                let title = documents.first { $0.id == documentID }?.title ?? documentID.rawValue
                roots.append(.init(id: "document:\(task.id.rawValue):\(documentID.rawValue):\(revisionID.rawValue)", kind: "document_review",
                                   subject: .documentReview(taskId: task.id, documentId: documentID, revisionId: revisionID),
                                   title: title, body: nil, taskId: task.id, requestId: task.requestId, agentId: task.agentId,
                                   revision: revision, order: interaction.order, pending: true, children: [entry], availableActions: []))
            } else {
                roots.append(entry)
            }
        }
        return roots.sorted { ($0.order, $0.id) < ($1.order, $1.id) }
    }

    func taskActions(_ id: DomainV2.TaskId) -> [DomainV2.AvailableAction] {
        guard let task = tasks.first(where: { $0.id == id }) else { return [] }
        if task.archived { return [action(.unarchiveTask)] }
        if task.state == .done || task.state == .cancelled { return [action(.reopenTask), action(.archiveTask)] }
        return [action(.cancelTask)]
    }

    private mutating func report(_ text: String, task: DomainV2.TaskId = designTask, nextID: inout UInt64) {
        let id = DomainV2.InteractionId(rawValue: "demo-\(nextID)")
        nextID += 1
        interactions.append(.init(attachments: [], id: id, scope: .task(taskId: task), author: .agent(agentId: Self.designAgent),
                                  recipient: .user, body: text, blocking: false, subject: nil, payload: .information(readBy: []),
                                  revision: revision + 1, order: revision * 10 + nextID))
    }

    private mutating func setTask(_ id: DomainV2.TaskId, _ change: (inout DomainV2.Task) -> Void) -> String? {
        guard let index = tasks.firstIndex(where: { $0.id == id }) else { return "Demo task not found" }
        change(&tasks[index])
        tasks[index].revision = revision + 1
        return nil
    }

    private func task(of interaction: DomainV2.InteractionId) -> DomainV2.TaskId? {
        guard let found = interactions.first(where: { $0.id == interaction }), case .task(let id) = found.scope else { return nil }
        return id
    }

    /// Applies one command; a message means the demo refused it, as the Mac would.
    mutating func apply(_ intent: DomainIntent, nextID: inout UInt64) -> String? {
        switch intent {
        case .markRead(let id):
            guard !settled.contains(id) else { return "This demo update is already read" }
            settled.insert(id)
        case .answer(let id, let optionID, let inputs, _):
            guard !settled.contains(id) else { return "This demo question is already answered" }
            settled.insert(id)
            var text = "Your demo answer: \(optionID)"
            if case .text(let value) = inputs["explanation"] { text += " — " + value }
            report(text, task: task(of: id) ?? Self.designTask, nextID: &nextID)
            if let task = task(of: id) { _ = setTask(task) { $0.state = .running } }
        case .acceptDelivery(let id):
            settled.insert(id)
            if let task = task(of: id) { _ = setTask(task) { $0.state = .done } }
        case .submitFeedback(let id, let body, _), .requestMoreContext(let id, let body, _), .replyToInformation(let id, let body, _):
            settled.insert(id)
            report("Your demo feedback: " + body, task: task(of: id) ?? Self.designTask, nextID: &nextID)
            if let task = task(of: id) { _ = setTask(task) { $0.state = .running } }
        case .authorizeMerge(let id, _):
            settled.insert(id)
            for index in pullRequests.indices where pullRequests[index].state == .open {
                pullRequests[index].state = .merged
                pullRequests[index].mergedHead = pullRequests[index].head
            }
            if let task = task(of: id) { _ = setTask(task) { $0.state = .closing } }
            report("Example pull request merged in demo.", nextID: &nextID)
        case .askAboutPullRequest(_, let head, let body, _):
            guard head == Self.head else { return "The example diff changed; reload it" }
            report("Your demo review: " + body, nextID: &nextID)
        case .requestConflictResolution:
            report("Demo assistant will resolve the sample conflicts.", nextID: &nextID)
        case .cancelTask(let id): return setTask(id) { $0.state = .cancelled }
        case .reopenTask(let id): return setTask(id) { $0.state = .running }
        case .archiveTask(let id, let archived): return setTask(id) { $0.archived = archived }
        case .closeRequest(let id):
            guard let index = requests.firstIndex(where: { $0.id == id }) else { return "Demo request not found" }
            requests[index].closed = true
        case .createRequest(let body, _, _, _, let attachments):
            let request = DomainV2.RequestId(rawValue: "demo-request-\(nextID)")
            let task = DomainV2.TaskId(rawValue: "demo-task-\(nextID)")
            nextID += 1
            requests.append(.init(id: request, body: body, closed: false, attachments: attachments))
            tasks.append(.init(id: task, requestId: request, agentId: Self.designAgent, title: body, state: .running,
                               revision: revision + 1, started: true, reviewPrs: false, finalPresentation: nil, prId: nil,
                               dependencies: [], stack: nil, documents: [:], archived: false))
            report("A synthetic task was created for: " + body, task: task, nextID: &nextID)
        case .messageAgent(_, let body, let attachments):
            let names = attachments.map(\.name)
            report("Your demo message: " + body + (names.isEmpty ? "" : "\nSample attachments: " + names.joined(separator: ", ")), nextID: &nextID)
        case .closeAgent(let id, _):
            guard agents.contains(where: { $0.id == id }) else { return "Demo node not found" }
            agents.removeAll { $0.id == id }
        case .retryOrchestratorRun, .cancelOrchestratorRun, .acknowledgeOrchestratorRun:
            return "The demo orchestrator has no runs"
        }
        return nil
    }
}
