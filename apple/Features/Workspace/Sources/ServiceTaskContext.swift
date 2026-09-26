import AlondraShared
import Foundation

/// Domain lookups are prepared outside the view's body; presentation only renders values.
public struct ServiceTaskContext {
    public var request: WorkRequest?
    public var requestTitle: String?
    public var project: String?
    public var now: String?
    public var requestTaskCount = 0
    public var pullRequestOwners: [UInt64: WorkTask] = [:]
    public var unreadReportIDs: Set<UInt64> = []
    public var inboxIDs: Set<String> = []
    public var dependencies: [WorkTask] = []
    public var dependents: [WorkTask] = []
    public var pullRequests: [PullRequest] = []
    public var reports: [Report] = []
    private var documentIDs: [UInt64] = []
    public func documents(in detail: TaskInFull?) -> [Report] {
        documentIDs.compactMap { id in detail?.documents?.first { $0.id == id } ?? reports.first { $0.id == id } }
    }
    public var pending: [InboxItem] = []
    public var nodeName: String?
    public init(board: TaskBoard, overview: Overview?, taskID: String, hiddenIDs: Set<String> = []) {
        guard let task = board.task(taskID) else { return }
        let presentation = ServiceTaskPresentation(overview: overview, hiddenIDs: hiddenIDs)
        request = board.request(task.request)
        requestTitle = request.map { presentation.title(of: $0) }
        project = presentation.project(of: task)
        now = presentation.waitingReason(for: task) ?? presentation.now(for: task)
        requestTaskCount = board.tree(for: task.request)?.rows.count ?? 0
        unreadReportIDs = Set(presentation.news.map(\.id))
        inboxIDs = Set(presentation.inbox.map(\.id))
        for owner in board.mergeOrder(around: task) {
            for pr in presentation.pullRequests(of: owner) { pullRequestOwners[pr.id] = owner }
        }
        dependencies = board.dependencies(of: task)
        dependents = board.dependents(of: task)
        pullRequests = board.mergeOrder(around: task).flatMap { owner in
            (overview?.prs ?? []).filter { owner.prs.contains($0.id) || $0.task == owner.id }.sorted(by: PullRequest.newestTrainsFirst)
        }
        documentIDs = task.documents
        reports = presentation.documents(of: task, in: nil)
        pending = presentation.items(forTask: task)
        nodeName = overview?.nodes.first { $0.sessionId == task.assignee?.sessionId }?.displayName
    }
}
