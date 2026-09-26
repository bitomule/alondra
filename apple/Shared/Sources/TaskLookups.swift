import Foundation

extension FleetStore {
    public func task(for item: InboxItem) -> WorkTask? {
        board.task(for: item)
    }

    /// The cards of the task that wait in the inbox.
    public func items(forTask task: WorkTask) -> [InboxItem] {
        inbox.filter { board.task(for: $0)?.id == task.id }
    }

    public func report(_ id: UInt64) -> Report? {
        news.first { $0.id == id } ?? overview?.reports?.first { $0.id == id }
    }

    public func isUnread(_ report: Report) -> Bool {
        news.contains { $0.id == report.id }
    }

    /// The dependency's PR a started task waits to see merged.
    public func awaitedPullRequest(of task: WorkTask) -> PullRequest? {
        task.awaiting.flatMap { id in overview?.prs?.first { $0.id == id } }
    }

    /// The task's PRs that are still open, bottom of their train first.
    public func pullRequests(of task: WorkTask) -> [PullRequest] {
        (overview?.prs ?? []).filter { task.prs.contains($0.id) || $0.task == task.id }.sorted(by: PullRequest.newestTrainsFirst)
    }

    public func node(of task: WorkTask) -> Node? {
        task.assignee.flatMap { node($0.sessionId) }
    }

    public func project(of task: WorkTask) -> String? {
        task.options?.project.flatMap(Self.named) ?? node(of: task)?.projectName ?? board.request(task.request)?.options?.project.flatMap(Self.named)
    }

    public func project(of request: WorkRequest) -> String? {
        request.options?.project.flatMap(Self.named) ?? board.tree(for: request.id)?.tasks.lazy.compactMap(project(of:)).first
    }

    /// The title the orchestrator gave it, else its only task's, else the first line of what the
    /// user asked (all a request being defined has), else its first task's.
    public func title(of request: WorkRequest) -> String {
        let tasks = board.tree(for: request.id)?.tasks ?? []
        if let title = request.title?.trimmingCharacters(in: .whitespacesAndNewlines), !title.isEmpty { return title }
        if tasks.count == 1 { return tasks[0].title }
        return request.headline ?? tasks.first?.title ?? String(localized: "Request", table: "AlondraShared", bundle: #bundle)
    }

    /// The task's documents: as its own route gave them, else those among the overview's newest
    /// reports.
    public func documents(of task: WorkTask, in details: TaskInFull?) -> [Report] {
        task.documents.compactMap { id in details?.documents?.first { $0.id == id } ?? report(id) }
    }

    public func request(of task: WorkTask) -> WorkRequest? {
        board.request(task.request)
    }

    /// What the node does now, per thread, asked of its own route: the overview does not carry
    /// it. An alondrad without the activity ledger gives only the main thread's tool in flight.
    public func threads(of node: Node) async -> [NodeActivity] {
        await threads(of: node) { try await client.nodeThreads($0).activity }
    }

    /// As `threads(of:)`, with `asking` in place of the node's route. When asking fails, what the
    /// route last said stays; until it has answered, the overview's tool in flight stands in.
    public func threads(of node: Node, asking: (String) async throws -> [NodeActivity]?) async -> [NodeActivity] {
        do {
            let asked = try await asking(node.sessionId)
            let threads = (asked ?? node.activity ?? node.toolThread).sorted { $0.since > $1.since }
            threadsShown = (node.sessionId, threads)
            return threads
        } catch {
            if let shown = threadsShown, shown.session == node.sessionId { return shown.threads }
            return (node.activity ?? node.toolThread).sorted { $0.since > $1.since }
        }
    }

    /// Moves when a node's detail asks its threads again from the start: the tick moved, so a
    /// state changed.
    public func threadsKey(of session: String) -> ThreadsKey {
        ThreadsKey(session: session, tick: overview?.tick ?? 0)
    }

    /// Feeds `show` what the node's threads do for as long as the caller runs (its detail shows):
    /// at once, then every `period`, since tool calls move them without moving the tick.
    public func followThreads(of session: String, every period: Duration = .seconds(10), _ show: ([NodeActivity]) -> Void) async {
        while !Task.isCancelled {
            if let node = node(session) {
                let threads = await threads(of: node)
                if Task.isCancelled { return }
                show(threads)
            }
            try? await Task.sleep(for: period)
        }
    }

    /// One line on what is happening to the task right now, or nil when there is nothing to add
    /// to its state.
    public func now(for task: WorkTask) -> String? {
        switch task.state {
        case .needsYou:
            guard let blocker = task.blocker, let title = questions.first(where: { $0.id == blocker.question })?.title else { return nil }
            switch blocker.kind {
            case "permission": return String(localized: "permission: \(title)", table: "AlondraShared", bundle: #bundle)
            case "action": return String(localized: "your turn: \(title)", table: "AlondraShared", bundle: #bundle)
            default: return String(localized: "question: \(title)", table: "AlondraShared", bundle: #bundle)
            }
        case .inReview:
            if let pr = pullRequests(of: task).first { return pr.progress(mergesAlone: task.mergesAlone) }
            guard let document = task.documents.reversed().lazy.compactMap(report).first else { return nil }
            return String(localized: "document: \(document.title)", table: "AlondraShared", bundle: #bundle)
        case .waitingDependency:
            if let awaited = awaitedPullRequest(of: task) {
                return String(localized: "waits for PR #\(String(awaited.number)) to be merged", table: "AlondraShared", bundle: #bundle)
            }
            if let pr = pullRequests(of: task).first, let below = pr.belowNumber {
                return String(localized: "PR #\(String(pr.number)) on top of #\(String(below))", table: "AlondraShared", bundle: #bundle)
            }
            return nil
        case .closing:
            return String(localized: "merged; the node does what the merge unblocks", table: "AlondraShared", bundle: #bundle)
        case .running, .done, .cancelled:
            return nil
        }
    }

    /// Why a task waits, in a sentence, for its detail.
    public func waitingReason(for task: WorkTask) -> String? {
        guard task.state == .waitingDependency else { return nil }
        if let awaited = awaitedPullRequest(of: task) {
            let title = awaited.task.flatMap { board.task($0)?.title } ?? awaited.title
            return String(localized: "It started, and what is left of it waits for PR #\(String(awaited.number)) of “\(title)” to be merged: it goes on once that PR is merged.", table: "AlondraShared", bundle: #bundle)
        }
        let pending = board.dependencies(of: task).filter { !$0.state.isFinished && $0.state != .inReview && $0.state != .closing }
        guard let first = pending.first else { return nil }
        return String(localized: "It starts once “\(first.title)” (\(first.state.label)) has its PR open, builds on top of it, and its own PR merges after that one.", table: "AlondraShared", bundle: #bundle)
    }

    private static func named(_ project: String) -> String? {
        project.isEmpty || project == "others" ? nil : project
    }
}


extension PullRequest {
    /// "PR #150 · waiting for checks, 2 of 3", for a task's line.
    public func progress(mergesAlone: Bool) -> String {
        let number = String(number)
        let checks = checks ?? []
        if hasConflicts {
            return String(localized: "PR #\(number) · conflicts", table: "AlondraShared", bundle: #bundle)
        }
        if checks.contains(where: \.failed) {
            return String(localized: "PR #\(number) · checks failed", table: "AlondraShared", bundle: #bundle)
        }
        if let below = belowNumber {
            return String(localized: "PR #\(number) · merge #\(String(below)) first", table: "AlondraShared", bundle: #bundle)
        }
        if checks.contains(where: \.running) {
            let passed = checks.count(where: \.passed)
            return String(localized: "PR #\(number) · waiting for checks, \(passed) of \(checks.count)", table: "AlondraShared", bundle: #bundle)
        }
        if mergesAlone {
            return String(localized: "PR #\(number) · Alondra merges it when the checks pass", table: "AlondraShared", bundle: #bundle)
        }
        return String(localized: "PR #\(number) · ready to merge", table: "AlondraShared", bundle: #bundle)
    }
}

extension WorkTask.Transition {
    /// What moved the task, in a few words.
    public var summary: String {
        switch event.kind {
        case "dependency_ready": String(localized: "a task it builds on has its PR open", table: "AlondraShared", bundle: #bundle)
        case "awaits_merge": String(localized: "the node has nothing left but the merge of a PR it builds on", table: "AlondraShared", bundle: #bundle)
        case "blocker_raised":
            switch event.blocker?.kind {
            case "permission": String(localized: "the node needs a permission", table: "AlondraShared", bundle: #bundle)
            case "action": String(localized: "the node asked you to do something", table: "AlondraShared", bundle: #bundle)
            default: String(localized: "the node asked you something", table: "AlondraShared", bundle: #bundle)
            }
        case "pr_opened": String(localized: "the node opened a PR", table: "AlondraShared", bundle: #bundle)
        case "document_sent": String(localized: "the node sent a document for review", table: "AlondraShared", bundle: #bundle)
        case "auto_merged": String(localized: "Alondra merged its PR", table: "AlondraShared", bundle: #bundle)
        case "final_report": String(localized: "the node reported it finished", table: "AlondraShared", bundle: #bundle)
        case "answered": String(localized: "you answered", table: "AlondraShared", bundle: #bundle)
        case "changes_requested": String(localized: "you asked for changes", table: "AlondraShared", bundle: #bundle)
        case "node_updated": String(localized: "the node pushed changes and let you know again", table: "AlondraShared", bundle: #bundle)
        case "merged": String(localized: "merged", table: "AlondraShared", bundle: #bundle)
        case "review_closed": String(localized: "the review closed without a merge", table: "AlondraShared", bundle: #bundle)
        case "reopen": String(localized: "reopened", table: "AlondraShared", bundle: #bundle)
        case "cancel": String(localized: "cancelled", table: "AlondraShared", bundle: #bundle)
        default: event.kind
        }
    }
}

extension WorkRequest {
    /// The first line of what the user asked, shortened.
    public var headline: String? {
        let line = (text ?? "").split(whereSeparator: \.isNewline).first.map { String($0).trimmingCharacters(in: .whitespaces) } ?? ""
        guard !line.isEmpty else { return nil }
        return line.count <= 90 ? line : String(line.prefix(89)).trimmingCharacters(in: .whitespaces) + "…"
    }
}
