import AlondraContracts
import Foundation

/// A self-contained Task detail resolved by explicit IDs from one projection revision.
/// Nothing here derives state: the Task's state, pending items and actions are the server's.
public struct DomainTaskDetail: Equatable, Sendable {
    public let task: DomainV2.Task
    public let request: DomainV2.Request?
    public let agent: DomainV2.Agent?
    public let pullRequest: DomainV2.PullRequest?
    public let documents: [TaskDocument]
    public let dependencies: [DomainV2.Task]
    public let dependents: [DomainV2.Task]
    public let requestTasks: [DomainV2.Task]
    public let pending: [DomainV2.InboxItem]
    public let finalPresentation: DomainV2.Interaction?
    public let mergeAuthorizations: [DomainV2.MergeAuthorization]
    public let stackReadiness: [DomainV2.StackReadiness]

    public init?(_ id: DomainV2.TaskId, in workspace: DomainWorkspace) {
        guard let task = workspace.task(id) else { return nil }
        self.task = task
        request = workspace.request(task.requestId)
        agent = workspace.agent(task.agentId)
        pullRequest = workspace.pullRequest(task.prId)
        documents = workspace.documents(of: task)
        dependencies = workspace.dependencies(of: task)
        dependents = workspace.dependents(of: id)
        requestTasks = workspace.tasks(in: task.requestId)
        pending = workspace.inboxItems(forTask: id).filter(\.pending)
        finalPresentation = workspace.interaction(task.finalPresentation)
        mergeAuthorizations = task.prId.map(workspace.mergeAuthorizations(for:)) ?? []
        stackReadiness = workspace.stackReadiness(for: id)
    }
}
