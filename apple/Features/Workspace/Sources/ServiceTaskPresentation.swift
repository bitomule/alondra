import AlondraShared
import Foundation

/// Read-only presentation lookup over an accepted snapshot; no networking or view state.
public struct ServiceTaskPresentation {
    public let overview: Overview?
    public let board: TaskBoard
    public let inbox: [InboxItem]
    public init(overview: Overview?, hiddenIDs: Set<String> = []) {
        self.overview = overview
        board = TaskBoard(requests: overview?.requests ?? [], tasks: overview?.tasks ?? [])
        let grouping = InboxGrouping(questions: (overview?.openQuestions ?? []).filter { $0.toOrchestrator != true }, reports: (overview?.news ?? []).filter { $0.toOrchestrator != true }, pullRequests: overview?.prs ?? [])
        inbox = (grouping.loose.map(InboxItem.question) + grouping.looseReports.map(InboxItem.report) + (overview?.prs ?? []).map(InboxItem.pullRequest)).filter { !hiddenIDs.contains($0.id) }
    }
    public var news: [Report] { overview?.news ?? [] }
    public var questions: [Question] { overview?.openQuestions ?? [] }
    public func node(_ id: String) -> Node? { overview?.nodes.first { $0.sessionId == id } }
    public func pending(for id: String) -> [InboxItem] { inbox.filter { $0.sessionId == id } }
    public var summary: String {
        let tasks = board.tasks.values.filter { !board.isHidden($0) }
        var parts = [String(localized: "\(board.activeTrees.count) requests")]
        let needsYou = tasks.count { $0.state == .needsYou }
        let inReview = tasks.count { $0.state == .inReview }
        let running = tasks.count { $0.state == .running }
        if needsYou > 0 { parts.append(String(localized: "\(needsYou) need you")) }
        if inReview > 0 { parts.append(String(localized: "\(inReview) in review")) }
        if running > 0 { parts.append(String(localized: "\(running) running")) }
        return parts.joined(separator: " · ")
    }

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
        return request.headline ?? tasks.first?.title ?? String(localized: "Request")
    }

    /// The task's documents: as its own route gave them, else those among the overview's newest
    /// reports.
    public func documents(of task: WorkTask, in details: TaskInFull?) -> [Report] {
        task.documents.compactMap { id in details?.documents?.first { $0.id == id } ?? report(id) }
    }

    public func request(of task: WorkTask) -> WorkRequest? {
        board.request(task.request)
    }

    /// One line on what is happening to the task right now, or nil when there is nothing to add
    /// to its state.
    public func now(for task: WorkTask) -> String? {
        switch task.state {
        case .needsYou:
            guard let blocker = task.blocker, let title = questions.first(where: { $0.id == blocker.question })?.title else { return nil }
            switch blocker.kind {
            case "permission": return String(localized: "permission: \(title)")
            case "action": return String(localized: "your turn: \(title)")
            default: return String(localized: "question: \(title)")
            }
        case .inReview:
            if let pr = pullRequests(of: task).first { return pr.progress(mergesAlone: task.mergesAlone) }
            guard let document = task.documents.reversed().lazy.compactMap(report).first else { return nil }
            return String(localized: "document: \(document.title)")
        case .waitingDependency:
            if let awaited = awaitedPullRequest(of: task) {
                return String(localized: "waits for PR #\(String(awaited.number)) to be merged")
            }
            if let pr = pullRequests(of: task).first, let below = pr.belowNumber {
                return String(localized: "PR #\(String(pr.number)) on top of #\(String(below))")
            }
            return nil
        case .closing:
            return String(localized: "merged; the node does what the merge unblocks")
        case .running, .done, .cancelled:
            return nil
        }
    }

    /// Why a task waits, in a sentence, for its detail.
    public func waitingReason(for task: WorkTask) -> String? {
        guard task.state == .waitingDependency else { return nil }
        if let awaited = awaitedPullRequest(of: task) {
            let title = awaited.task.flatMap { board.task($0)?.title } ?? awaited.title
            return String(localized: "It started, and what is left of it waits for PR #\(String(awaited.number)) of “\(title)” to be merged: it goes on once that PR is merged.")
        }
        let pending = board.dependencies(of: task).filter { !$0.state.isFinished && $0.state != .inReview && $0.state != .closing }
        guard let first = pending.first else { return nil }
        return String(localized: "It starts once “\(first.title)” (\(first.state.label)) has its PR open, builds on top of it, and its own PR merges after that one.")
    }

    private static func named(_ project: String) -> String? {
        project.isEmpty || project == "others" ? nil : project
    }
}
