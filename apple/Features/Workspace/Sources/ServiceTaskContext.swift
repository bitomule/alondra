import AlondraContracts
import AlondraDomain
import AlondraShared
import Foundation

/// Domain lookups are prepared outside the view's body; presentation only renders values.
public struct ServiceTaskContext {
    public var request: DomainV2.Request?
    public var requestTitle: String?
    public var project: String?
    public var now: String?
    public var requestTaskCount = 0
    public var dependencies: [DomainV2.Task] = []
    public var dependents: [DomainV2.Task] = []
    public var pullRequests: [DomainV2.PullRequest] = []
    public var observations: [DomainV2.PrId: PullRequest] = [:]
    public var documents: [TaskDocument] = []
    public var pending: [DomainV2.InboxItem] = []
    public var agentName: String?
    public var history: [DomainV2.TaskTransition] = []

    public init() {}

    public init(presentation: ServiceTaskPresentation, taskID: DomainV2.TaskId) {
        guard let task = presentation.board.task(taskID) else { return }
        let board = presentation.board
        request = board.request(task.requestId)
        requestTitle = request.map { presentation.title(of: $0) }
        project = presentation.project(of: task)
        now = presentation.waitingReason(for: task) ?? presentation.now(for: task)
        requestTaskCount = board.tree(for: task.requestId)?.rows.count ?? 0
        dependencies = board.dependencies(of: task)
        dependents = board.dependents(of: task)
        pullRequests = presentation.pullRequests(of: task)
        for pr in pullRequests { observations[pr.id] = presentation.observation(of: pr) }
        documents = presentation.documents(of: task)
        pending = presentation.items(forTask: task)
        agentName = presentation.node(task.agentId)?.displayName ?? presentation.agent(task.agentId)?.name
        history = presentation.runtime?.task(task.id)?.history ?? []
    }
}
