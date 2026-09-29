import AlondraContracts
import AlondraDomain
import AlondraShared
import Foundation

/// Domain lookups are prepared outside the view's body; presentation only renders values.
public struct ServiceTaskContext {
    public var request: DomainV2.Request?
    public var requestTitle: String?
    public var requestCreatedAt: Int64?
    public var project: String?
    public var now: String?
    public var stateSince: Int64?
    public var effort: String?
    public var requestTaskCount = 0
    public var dependencies: [DomainV2.Task] = []
    public var dependents: [DomainV2.Task] = []
    /// The PRs of the task and of those it stacks with, in the order they merge.
    public var pullRequests: [DomainV2.PullRequest] = []
    public var pullRequestOwners: [DomainV2.PrId: DomainV2.Task] = [:]
    /// The inbox card of each PR that waits for you.
    public var pullRequestItems: [DomainV2.PrId: String] = [:]
    public var observations: [DomainV2.PrId: PullRequest] = [:]
    public var documents: [TaskDocument] = []
    /// The inbox card asking you to review each document, while one waits.
    public var documentItems: [DomainV2.DocumentId: String] = [:]
    public var pending: [DomainV2.InboxItem] = []
    public var agentName: String?
    public var history: [DomainV2.TaskTransition] = []

    public init() {}

    public init(presentation: ServiceTaskPresentation, taskID: DomainV2.TaskId) {
        guard let task = presentation.board.task(taskID) else { return }
        let board = presentation.board
        request = board.request(task.requestId)
        requestTitle = request.map { presentation.title(of: $0) }
        requestCreatedAt = request.flatMap { presentation.createdAt(of: $0) }
        project = presentation.project(of: task)
        now = presentation.waitingReason(for: task) ?? presentation.now(for: task)
        stateSince = presentation.stateSince(of: task)
        effort = presentation.runtime?.task(task.id)?.effort.flatMap { $0.isEmpty ? nil : $0 }
        requestTaskCount = board.tree(for: task.requestId)?.rows.count ?? 0
        dependencies = board.dependencies(of: task)
        dependents = board.dependents(of: task)
        for owner in board.mergeOrder(around: task) {
            guard let pr = presentation.pullRequest(of: owner), pr.state == .open,
                  pullRequestOwners[pr.id] == nil else { continue }
            pullRequests.append(pr)
            pullRequestOwners[pr.id] = owner
            observations[pr.id] = presentation.observation(of: pr)
            pullRequestItems[pr.id] = presentation.inboxItem(for: pr.id)?.id
        }
        documents = presentation.documents(of: task)
        for document in documents {
            documentItems[document.id] = presentation.reviewItem(for: document.id, of: task.id)?.id
        }
        pending = presentation.items(forTask: task)
        agentName = presentation.node(task.agentId)?.displayName ?? presentation.agent(task.agentId)?.name
        history = presentation.runtime?.task(task.id)?.history ?? []
    }
}
