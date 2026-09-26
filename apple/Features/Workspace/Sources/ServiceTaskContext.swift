import AlondraShared
import Foundation

/// Domain lookups are prepared outside the view's body; presentation only renders values.
public struct ServiceTaskContext {
    public var request: WorkRequest?
    public var dependencies: [WorkTask] = []
    public var dependents: [WorkTask] = []
    public var pullRequests: [PullRequest] = []
    public var reports: [Report] = []
    public var pending: [InboxItem] = []
    public var nodeName: String?
    public init(board: TaskBoard, overview: Overview?, taskID: String) {
        guard let task = board.task(taskID) else { return }
        request = board.request(task.request)
        dependencies = board.dependencies(of: task)
        dependents = board.dependents(of: task)
        pullRequests = board.mergeOrder(around: task).flatMap { owner in
            (overview?.prs ?? []).filter { owner.prs.contains($0.id) || $0.task == owner.id }.sorted(by: PullRequest.newestTrainsFirst)
        }
        reports = (overview?.reports ?? overview?.news ?? []).filter { task.reports.contains($0.id) }
        let grouping = InboxGrouping(questions: overview?.openQuestions ?? [], reports: overview?.news ?? [], pullRequests: overview?.prs ?? [])
        let inbox = grouping.loose.map(InboxItem.question) + grouping.looseReports.map(InboxItem.report) + (overview?.prs ?? []).map(InboxItem.pullRequest)
        pending = inbox.filter { board.task(for: $0)?.id == task.id }
        nodeName = overview?.nodes.first { $0.sessionId == task.assignee?.sessionId }?.displayName
    }
}
