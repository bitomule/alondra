import AlondraContracts
import AlondraDomain
import AlondraShared
import Foundation

/// Read-only presentation lookup over one server view; no networking or view state. Every
/// relation follows an explicit ID; project, dates and PR checks come from the runtime read.
public struct ServiceTaskPresentation {
    public let workspace: DomainWorkspace?
    public let board: TaskBoard
    public let inbox: [DomainV2.InboxItem]

    public init(workspace: DomainWorkspace?, hiddenIDs: Set<String> = []) {
        self.workspace = workspace
        board = workspace.map(TaskBoard.init) ?? .empty
        inbox = (workspace?.roots ?? []).filter { !hiddenIDs.contains($0.id) }
    }

    public var runtime: DomainV2.Runtime? { workspace?.runtime }

    public func agent(_ id: DomainV2.AgentId?) -> DomainV2.Agent? { workspace?.agent(id) }

    public func node(_ id: DomainV2.AgentId?) -> Node? { runtime?.node(id) }

    /// The inbox cards that wait on this agent.
    public func pending(for agent: DomainV2.AgentId) -> [DomainV2.InboxItem] {
        inbox.filter { $0.agentId == agent }
    }

    public var summary: String {
        let tasks = board.tasks.values.filter { !$0.archived }
        var parts = [String(localized: "\(board.activeTrees.count) requests")]
        let needsYou = tasks.count { $0.state == .needsYou }
        let inReview = tasks.count { $0.state == .inReview }
        let running = tasks.count { $0.state == .running }
        if needsYou > 0 { parts.append(String(localized: "\(needsYou) need you")) }
        if inReview > 0 { parts.append(String(localized: "\(inReview) in review")) }
        if running > 0 { parts.append(String(localized: "\(running) running")) }
        return parts.joined(separator: " · ")
    }

    public func task(for item: DomainV2.InboxItem) -> DomainV2.Task? { item.taskId.flatMap(board.task) }

    /// The cards of the task that wait in the inbox.
    public func items(forTask task: DomainV2.Task) -> [DomainV2.InboxItem] {
        inbox.filter { $0.taskId == task.id }
    }

    public func pullRequest(of task: DomainV2.Task) -> DomainV2.PullRequest? { workspace?.pullRequest(task.prId) }

    /// The inbox card of this PR, while one waits for you.
    public func inboxItem(for pr: DomainV2.PrId) -> DomainV2.InboxItem? {
        inbox.first { $0.subject == .pullRequest(prId: pr) }
    }

    /// The inbox card asking you to review this document of the task, while one waits.
    public func reviewItem(for document: DomainV2.DocumentId, of task: DomainV2.TaskId) -> DomainV2.InboxItem? {
        inbox.first { item in
            if case let .documentReview(taskId, documentId, _) = item.subject { return taskId == task && documentId == document }
            return false
        }
    }

    /// What GitHub last said about the PR: title, checks, conflicts.
    public func observation(of pr: DomainV2.PullRequest?) -> DomainV2.PullRequestRuntime? { runtime?.pullRequest(pr?.id) }

    /// The open PR this one is stacked on, which merges first.
    public func stackedOn(_ pr: DomainV2.PullRequest) -> DomainV2.PullRequest? { workspace?.stackedOn(pr) }

    /// The task's PR, then the PRs of the tasks it stacks on, bottom last.
    public func pullRequests(of task: DomainV2.Task) -> [DomainV2.PullRequest] {
        var seen: Set<DomainV2.TaskId> = []
        var current: DomainV2.Task? = task
        var result: [DomainV2.PullRequest] = []
        while let next = current, seen.insert(next.id).inserted {
            if let pr = pullRequest(of: next), pr.state == .open { result.append(pr) }
            current = next.stack.flatMap { board.task($0.upstreamTask) }
        }
        return result
    }

    public func sidebarSubtitle(for task: DomainV2.Task) -> String {
        [project(of: task), now(for: task) ?? task.state.label].compactMap { $0 }.joined(separator: " · ")
    }

    public func sidebarSubtitle(for request: DomainV2.Request) -> String {
        [project(of: request), definingLabel(of: request)].compactMap { $0 }.joined(separator: " · ")
    }

    /// Where a schedule's last run stands, from the request it created: the state key the
    /// schedule labels use, and the agents working on it.
    public func lastRun(of schedule: ScheduledTask) -> (state: String?, agents: [String]) {
        guard let id = schedule.lastRequest, let request = board.request(DomainV2.RequestId(rawValue: id)) else { return (nil, []) }
        let tasks = board.tree(for: request.id)?.tasks ?? []
        let state = if tasks.isEmpty { request.closed ? "done" : "defining" }
            else if request.closed || tasks.allSatisfy({ $0.state.isFinished }) { "done" }
            else { "in_progress" }
        var agents: [String] = []
        for task in tasks {
            if let name = node(task.agentId)?.displayName ?? agent(task.agentId)?.name, !agents.contains(name) { agents.append(name) }
        }
        return (state, agents)
    }

    /// The state of a request the orchestrator has not split into Tasks.
    public func definingLabel(of request: DomainV2.Request) -> String {
        request.closed
            ? String(localized: "finished without tasks")
            : String(localized: "the orchestrator is splitting it")
    }

    public func createdAt(of request: DomainV2.Request) -> Int64? { runtime?.request(request.id)?.createdAt }

    public func stateSince(of task: DomainV2.Task) -> Int64? { runtime?.task(task.id)?.stateSince }

    public func project(of task: DomainV2.Task) -> String? {
        (runtime?.task(task.id)?.project).flatMap(Self.named)
            ?? node(task.agentId)?.projectName
            ?? board.request(task.requestId).flatMap(project(of:))
    }

    /// A request carries no project of its own; its Tasks do.
    public func project(of request: DomainV2.Request) -> String? {
        board.tree(for: request.id)?.tasks.lazy.compactMap { (self.runtime?.task($0.id)?.project).flatMap(Self.named) }.first
    }

    /// Its only task's title, else the first line of what the user asked, else its first task's.
    public func title(of request: DomainV2.Request) -> String {
        Self.title(of: request, in: board)
    }

    public static func title(of request: DomainV2.Request, in board: TaskBoard) -> String {
        if let title = request.title?.trimmingCharacters(in: .whitespacesAndNewlines), !title.isEmpty { return title }
        let tasks = board.tree(for: request.id)?.tasks ?? []
        if tasks.count == 1 { return tasks[0].title }
        return request.headline ?? tasks.first?.title ?? String(localized: "Request")
    }

    public func documents(of task: DomainV2.Task) -> [TaskDocument] { workspace?.documents(of: task) ?? [] }

    public func taskLine(_ task: DomainV2.Task) -> String {
        guard let request = request(of: task) else { return task.state.label }
        return "\(task.state.label) · \(String(localized: "from “\(title(of: request))”"))"
    }

    public func request(of task: DomainV2.Task) -> DomainV2.Request? { board.request(task.requestId) }

    /// One line on what is happening to the task right now, or nil when there is nothing to add
    /// to its state.
    public func now(for task: DomainV2.Task) -> String? {
        switch task.state {
        case .needsYou:
            guard let item = items(forTask: task).first(where: \.pending) else { return nil }
            return String(localized: "question: \(item.title)")
        case .inReview:
            if let pr = pullRequest(of: task), let observation = observation(of: pr) {
                return observation.progress(number: pr.number, stackedOn: stackedOn(pr)?.number, mergesAlone: !task.reviewPrs)
            }
            guard let document = documents(of: task).last else { return nil }
            return String(localized: "document: \(document.document.title)")
        case .waitingDependency:
            if let parent = task.stack.flatMap({ board.task($0.upstreamTask) }), let pr = pullRequest(of: parent) {
                return String(localized: "waits for PR #\(String(pr.number)) to be merged")
            }
            return nil
        case .closing:
            return String(localized: "merged; the node does what the merge unblocks")
        case .notStarted, .running, .done, .cancelled:
            return nil
        }
    }

    /// Why a task waits, in a sentence, for its detail.
    public func waitingReason(for task: DomainV2.Task) -> String? {
        guard task.state == .waitingDependency else { return nil }
        if let parent = task.stack.flatMap({ board.task($0.upstreamTask) }), let pr = pullRequest(of: parent) {
            return String(localized: "It started, and what is left of it waits for PR #\(String(pr.number)) of “\(parent.title)” to be merged: it goes on once that PR is merged.")
        }
        let pending = board.dependencies(of: task).filter { !$0.state.isFinished && $0.state != .inReview && $0.state != .closing }
        guard let first = pending.first else { return nil }
        return String(localized: "It starts once “\(first.title)” (\(first.state.label)) has its PR open, builds on top of it, and its own PR merges after that one.")
    }

    private static func named(_ project: String) -> String? {
        project.isEmpty || project == "others" ? nil : project
    }
}
