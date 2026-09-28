import AlondraAttachmentsData
import AlondraCommands
import AlondraAttachmentsFeature
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraPullRequestsFeature
import AlondraQuestionsFeature
import AlondraShared
import AlondraTasksFeature
import AlondraTriageFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation

/// Synthetic session: no network, credentials, persistence, notifications or filesystem I/O.
/// Every reset creates a new actor; closing this actor invalidates captured dependencies.
public actor DemoWorkspace {
    public nonisolated let identity: TriageIdentity
    private var root: [String: Any]
    private var current: Overview
    private var active = true
    private var observers: [UUID: AsyncStream<Overview?>.Continuation] = [:]
    private var files: [UUID: (String, Data)] = [:]
    private var schedules: [[String: Any]] = []
    private var nextID: UInt64 = 10
    private let now: Int64

    public init(now: Date = .now) throws {
        self.now = Int64(now.timeIntervalSince1970 * 1000)
        let root = DemoFixture.make(now: self.now)
        current = try DemoFixture.decode(root)
        self.root = root
        identity = TriageIdentity(server: URL(string: "demo://workspace")!, deviceID: "demo",
                                  credentialRevision: UUID().uuidString, generation: 0)
    }
    public func snapshot() -> Overview? { active ? current : nil }
    public func close() {
        active = false
        files.removeAll(); schedules.removeAll(); root.removeAll()
        for continuation in observers.values { continuation.yield(nil); continuation.finish() }
        observers.removeAll()
    }
    public func snapshots() -> AsyncStream<Overview?> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<Overview?>.makeStream(bufferingPolicy: .bufferingNewest(1))
        guard active else { continuation.finish(); return stream }
        observers[id] = continuation
        continuation.yield(current)
        continuation.onTermination = { [weak self] _ in Task { await self?.removeObserver(id) } }
        return stream
    }
    private func removeObserver(_ id: UUID) { observers.removeValue(forKey: id) }
    private func publish() throws {
        root["tick"] = current.tick + 1
        current = try DemoFixture.decode(root)
        for continuation in observers.values { continuation.yield(current) }
    }
    private func ensureActive() throws { guard active else { throw CancellationError() } }
    private func records(_ key: String) -> [[String: Any]] { root[key] as? [[String: Any]] ?? [] }
    private func remove(_ key: String, id: UInt64) { root[key] = records(key).filter { ($0["id"] as? NSNumber)?.uint64Value != id } }
    private func report(_ text: String, session: String = "demo-design") {
        let report: [String: Any] = ["id": nextID, "sessionId": session, "nodeName": "Demo assistant", "title": "Demo update",
            "body": text, "links": [], "attachments": [], "kind": "notice", "createdAt": now + Int64(nextID)]
        nextID += 1
        root["news"] = records("news") + [report]
        root["reports"] = records("reports") + [report]
    }
    public func storeAttachment(id: UUID, name: String, data: Data) throws -> String {
        try ensureActive()
        if let existing = files[id], existing.0 != name || existing.1 != data { throw DemoError.attachmentConflict }
        files[id] = (name, data)
        return "demo-attachment-" + id.uuidString
    }
    private func inboxUpdates() -> AsyncStream<TriageInboxUpdate> {
        AsyncStream { continuation in
            let task = Task {
                for await snapshot in self.snapshots() {
                    continuation.yield(TriageInboxUpdate(identity: self.identity, overview: snapshot, connection: .online))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
    private func mutate(_ kind: CommandKind, identity: TriageIdentity) -> TriageMutationOutcome {
        guard active, identity == self.identity else { return .obsolete }
        do {
            switch kind {
            case .markRead(let id):
                guard records("news").contains(where: { ($0["id"] as? NSNumber)?.uint64Value == id }) else { return .rejected("Demo report is already read") }
                remove("news", id: id)
            case .dismissPullRequest(let id):
                guard records("prs").contains(where: { ($0["id"] as? NSNumber)?.uint64Value == id }) else { return .rejected("Demo PR is no longer open") }
                remove("prs", id: id)
            default: return .rejected("Unsupported demo action")
            }
            try publish(); return .confirmed
        } catch { return .rejected(error.localizedDescription) }
    }
    private func question(_ operation: QuestionOperation) -> QuestionWorkOutcome {
        guard active else { return .obsolete }
        switch operation {
        case .answer(let id, let text, _, let attachments):
            guard records("openQuestions").contains(where: { ($0["id"] as? NSNumber)?.uint64Value == id }) else { return .conflict("Demo question is already answered") }
            guard let summary = attachmentSummary(attachments) else { return .failed("Demo attachment is no longer available") }
            remove("openQuestions", id: id); report("Your demo answer: " + text + summary)
        case .moreContext(let id, let note, _):
            report("Extra demo context: compare the two layouts for clarity. " + note)
            remove("openQuestions", id: id)
        case .reviewReport(let id, let verdict): remove("news", id: id); report("Demo review: " + verdict.rawValue)
        }
        do { try publish(); return .confirmed } catch { return .failed(error.localizedDescription) }
    }
    private func feedback(_ request: ReportFeedbackRequest, identity: QuestionWorkIdentity) -> QuestionWorkOutcome {
        guard active, identity.credentialRevision == self.identity.credentialRevision,
              identity.server == self.identity.server, identity.deviceID == self.identity.deviceID else { return .obsolete }
        guard let summary = attachmentSummary(request.attachmentIDs) else { return .failed("Demo attachment is no longer available") }
        remove("news", id: request.reportID); report("Your demo feedback: " + request.text + summary, session: request.sessionID)
        do { try publish(); return .confirmed } catch { return .failed(error.localizedDescription) }
    }
    private func attachmentSummary(_ ids: [String]) -> String? {
        var names: [String] = []
        for id in ids {
            guard let file = files.first(where: { "demo-attachment-" + $0.key.uuidString == id }) else { return nil }
            names.append(file.value.0)
        }
        return names.isEmpty ? "" : "\nSample attachments: " + names.joined(separator: ", ")
    }
    private func document(_ path: String) throws -> String {
        try ensureActive()
        guard path == "demo://welcome.md" else { throw DemoError.missingItem }
        return DemoFixture.document
    }
    private func withdraw(_ id: String) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        root["requests"] = records("requests").filter { $0["id"] as? String != id }
        do { try publish(); return .confirmed }
        catch { return .rejected(error.localizedDescription) }
    }
    private func task(_ id: String, state: String) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        guard records("tasks").contains(where: { $0["id"] as? String == id }) else { return .rejected("Demo task not found") }
        root["tasks"] = records("tasks").map { item in
            guard item["id"] as? String == id else { return item }
            var next = item; next["state"] = state; return next
        }
        do { try publish(); return .confirmed } catch { return .rejected(error.localizedDescription) }
    }
    private func details(_ task: WorkTask) -> TaskInFull? {
        guard active, let item = records("tasks").first(where: { $0["id"] as? String == task.id }) else { return nil }
        return try? DemoFixture.decode(["task": item, "reports": records("reports")])
    }
    private func archive() -> TaskMutationOutcome {
        guard active else { return .obsolete }
        root["tasks"] = records("tasks").filter { !["done", "cancelled"].contains($0["state"] as? String ?? "") }
        do { try publish(); return .confirmed } catch { return .rejected(error.localizedDescription) }
    }
    private func pr(_ id: UInt64, state: String, message: String) -> PRMutationOutcome {
        guard active else { return .obsolete }
        guard records("prs").contains(where: { ($0["id"] as? NSNumber)?.uint64Value == id }) else { return .closed }
        if state != "open" { remove("prs", id: id) }
        report(message)
        do { try publish(); return .confirmed } catch { return .rejected(error.localizedDescription) }
    }
    private func reviewPR(_ id: UInt64, text: String, attachments: [PRReviewAttachment], kind: PRReviewKind, head: String?) -> PRMutationOutcome {
        guard active else { return .obsolete }
        guard head == nil || head == String(repeating: "d", count: 40) else { return .rejected("The example diff changed; reload it") }
        do {
            for file in attachments { _ = try storeAttachment(id: file.id, name: file.name, data: file.data) }
        } catch { return .rejected(error.localizedDescription) }
        let summary = text + (attachments.isEmpty ? "" : "\nSample attachments: " + attachments.map(\.name).joined(separator: ", "))
        return pr(id, state: kind == .ask ? "open" : "changes", message: "Your demo review: " + summary)
    }
    private func node(_ id: String, message: NodeMessage?) -> NodeMutationOutcome {
        guard active else { return .obsolete }
        guard records("nodes").contains(where: { $0["sessionId"] as? String == id }) else { return .rejected("Demo node not found") }
        if let message {
            do {
                for file in message.files { _ = try storeAttachment(id: file.id, name: file.name, data: file.data) }
            } catch { return .rejected(error.localizedDescription) }
            report("Your demo message: " + message.text + (message.files.isEmpty ? "" : "\nSample attachments: " + message.files.map(\.name).joined(separator: ", ")), session: id)
        }
        else { root["nodes"] = records("nodes").filter { $0["sessionId"] as? String != id } }
        do { try publish(); return .confirmed } catch { return .rejected(error.localizedDescription) }
    }
    private func create(_ body: NewRequestBody) -> CreateRequestResult {
        guard active else { return .cancelled }
        guard body.attachments.allSatisfy({ value in files.keys.contains { "demo-attachment-" + $0.uuidString == value } }) else {
            return .failed("The demo attachment is no longer available")
        }
        let requestID = "demo-request-\(nextID)", taskID = "demo-task-\(nextID)"
        nextID += 1
        let attachments: [[String: Any]] = body.attachments.compactMap { id in
            guard let file = files.first(where: { "demo-attachment-" + $0.key.uuidString == id }) else { return nil }
            return ["name": file.value.0, "sourcePath": "demo://attachments/" + file.key.uuidString]
        }
        root["requests"] = records("requests") + [["id": requestID, "text": body.text, "title": body.text, "createdAt": now,
            "state": "in_progress", "tasks": [taskID], "attachments": attachments]]
        root["tasks"] = records("tasks") + [["id": taskID, "request": requestID, "title": body.text, "dependsOn": [],
            "prs": [], "documents": [], "notices": [], "state": "running", "stateSince": now, "createdAt": now]]
        report("A synthetic task was created for: " + body.text)
        do { try publish(); return .confirmed } catch { return .failed(error.localizedDescription) }
    }
    private func scheduleList() throws -> [ScheduledTask] { try ensureActive(); return try DemoFixture.decode(schedules) }
    private func saveSchedule(_ draft: ScheduledTaskDraft, id: String?) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        do {
            guard var item = try JSONSerialization.jsonObject(with: JSONEncoder().encode(draft)) as? [String: Any] else { throw DemoError.missingItem }
            item["disallowedTools"] = item.removeValue(forKey: "disallowed_tools")
            item["startAt"] = item.removeValue(forKey: "start_at")
            let id = id ?? "demo-schedule-\(nextID)"; nextID += 1
            item["id"] = id; item["status"] = draft.enabled ? "scheduled" : "paused"
            schedules.removeAll { $0["id"] as? String == id }; schedules.append(item)
            _ = try scheduleList(); return .confirmed
        } catch { return .rejected(error.localizedDescription) }
    }
    private func runSchedule(_ id: String) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        guard let item = schedules.first(where: { $0["id"] as? String == id }) else { return .rejected("Demo schedule not found") }
        let result = create(NewRequestBody(text: item["text"] as? String ?? "Demo schedule", project: nil, options: [:], issue: nil))
        if case .confirmed = result { return .confirmed }; return .rejected("Could not create demo task")
    }
    private func deleteSchedule(_ id: String) -> TaskMutationOutcome {
        guard active else { return .obsolete }
        schedules.removeAll { $0["id"] as? String == id }; return .confirmed
    }
    private func options(project: String?) throws -> OptionsCatalog { try ensureActive(); return try DemoFixture.decode(DemoFixture.options(project: project)) }
    private func issues() throws -> IssueList { try ensureActive(); return try DemoFixture.decode(["issues": []]) }
    private func scope() -> NewWorkSubmissionScope? {
        guard active else { return nil }
        return NewWorkSubmissionScope(upload: { try await self.storeAttachment(id: $0.id, name: $0.name, data: $0.data) },
            isCurrent: { await self.active }, create: { await self.create($0) })
    }
    public nonisolated func install(into values: inout DependencyValues) {
        values.attachmentImport = AttachmentImportClient(file: { try AttachmentImporter.file($0) }, photo: { try await AttachmentImporter.photo($0) }, camera: { try AttachmentImporter.camera($0) }, thumbnail: { await AttachmentImporter.thumbnail($0) })
        values.attachmentUpload = AttachmentUploadClient(upload: {
            try await self.storeAttachment(id: $0.id, name: $0.name, data: $0.data)
        })
        values.triageService = TriageService(observe: { await self.inboxUpdates() }, mutate: { kind, _, identity in await self.mutate(kind, identity: identity) })
        values.serviceTasksClient = ServiceTasksClient(refresh: { try? await self.publish() }, observe: { await self.snapshots() })
        values.questionWorkService = QuestionWorkService(submit: { await self.question($0) },
            submitReportFeedback: { await self.feedback($0, identity: $1) }, document: { try await self.document($0) })
        values.taskOperations = TaskOperations(withdraw: { await self.withdraw($0.id) }, cancel: { await self.task($0.id, state: "cancelled") },
            reopen: { await self.task($0.id, state: "running") }, archiveCompleted: { await self.archive() }, details: { await self.details($0) })
        values.scheduleOperations = ScheduleOperations(list: { try await self.scheduleList() }, save: { await self.saveSchedule($0, id: $1) },
            run: { await self.runSchedule($0) }, delete: { await self.deleteSchedule($0) }, projects: { ["Demo Garden"] }, options: { try await self.options(project: $0).options })
        values.nodeOperations = NodeOperations(send: { await self.node($0, message: $1) }, close: { await self.node($0, message: nil) }, threads: { _ in [] })
        values.pullRequestOperations = PullRequestOperations(diff: { _ in try await self.ensureActive(); return PRDiffSnapshot(diff: DemoFixture.diff, headSHA: String(repeating: "d", count: 40)) },
            merge: { id, _ in await self.pr(id, state: "merged", message: "Example pull request merged in demo.") },
            resolveConflicts: { await self.pr($0, state: "open", message: "Demo assistant will resolve the sample conflicts.") },
            setAside: { await self.pr($0, state: "dismissed", message: "Example pull request set aside.") },
            review: { id, text, files, kind, head in await self.reviewPR(id, text: text, attachments: files, kind: kind, head: head) })
        values.newWorkOperations = NewWorkOperations(projects: { ["Demo Garden"] }, options: { try await self.options(project: $0) },
            issues: { _ in try await self.issues() }, captureSubmissionScope: { await self.scope() })
        values.serviceCommandRecovery = ServiceCommandRecoveryClient(observe: { AsyncStream { $0.finish() } }, observeFeedback: { AsyncStream { $0.finish() } })
    }
    private enum DemoError: Error { case missingItem, attachmentConflict }
}
