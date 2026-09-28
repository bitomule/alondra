import AlondraContracts
import Foundation

/// One coherent server projection with ID lookups. Grouping, hierarchy, order and action
/// availability are the server's: this type only follows explicit relations by ID and never
/// infers ownership from URLs, titles, dates or arrays.
public struct DomainWorkspace: Equatable, Sendable {
    public let projection: DomainV2.DomainProjection
    private let tasks: [DomainV2.TaskId: DomainV2.Task]
    private let agents: [DomainV2.AgentId: DomainV2.Agent]
    private let requests: [DomainV2.RequestId: DomainV2.Request]
    private let interactions: [DomainV2.InteractionId: DomainV2.Interaction]
    private let pullRequests: [DomainV2.PrId: DomainV2.PullRequest]
    private let documents: [DomainV2.DocumentId: DomainV2.Document]
    private let revisions: [DomainV2.DocumentRevisionId: DomainV2.DocumentRevision]
    private let items: [String: DomainV2.InboxItem]
    private let rootOfItem: [String: String]

    public init(_ projection: DomainV2.DomainProjection) {
        self.projection = projection
        tasks = Self.index(projection.tasks, by: \.id)
        agents = Self.index(projection.agents, by: \.id)
        requests = Self.index(projection.requests, by: \.id)
        interactions = Self.index(projection.interactions, by: \.id)
        pullRequests = Self.index(projection.pullRequests, by: \.id)
        documents = Self.index(projection.documents, by: \.id)
        revisions = Self.index(projection.documentRevisions, by: \.id)
        var items: [String: DomainV2.InboxItem] = [:]
        var rootOfItem: [String: String] = [:]
        func visit(_ item: DomainV2.InboxItem, root: String) {
            if items[item.id] == nil { items[item.id] = item }
            if rootOfItem[item.id] == nil { rootOfItem[item.id] = root }
            for child in item.children { visit(child, root: root) }
        }
        for root in projection.inbox { visit(root, root: root.id) }
        self.items = items
        self.rootOfItem = rootOfItem
    }

    private static func index<Key: Hashable, Value>(_ values: [Value], by key: KeyPath<Value, Key>) -> [Key: Value] {
        Dictionary(values.map { ($0[keyPath: key], $0) }, uniquingKeysWith: { first, _ in first })
    }

    public static func == (lhs: Self, rhs: Self) -> Bool { lhs.projection == rhs.projection }

    public var epoch: DomainV2.DataEpoch { projection.dataEpoch }
    public var revision: DomainV2.Revision { projection.revision }
    public var roots: [DomainV2.InboxItem] { projection.inbox }
    public var counts: DomainV2.InboxCounts { projection.counts }

    public func item(_ id: String) -> DomainV2.InboxItem? { items[id] }

    /// The card that currently contains `id`: the item itself when it is a root.
    public func root(containing id: String) -> DomainV2.InboxItem? { rootOfItem[id].flatMap { items[$0] } }

    public func item(for interaction: DomainV2.InteractionId) -> DomainV2.InboxItem? {
        items["interaction:\(interaction.rawValue)"]
            ?? items.values.first { $0.subject == .interaction(interactionId: interaction) }
    }

    public func task(_ id: DomainV2.TaskId?) -> DomainV2.Task? { id.flatMap { tasks[$0] } }
    public func agent(_ id: DomainV2.AgentId?) -> DomainV2.Agent? { id.flatMap { agents[$0] } }
    public func request(_ id: DomainV2.RequestId?) -> DomainV2.Request? { id.flatMap { requests[$0] } }
    public func interaction(_ id: DomainV2.InteractionId?) -> DomainV2.Interaction? { id.flatMap { interactions[$0] } }
    public func pullRequest(_ id: DomainV2.PrId?) -> DomainV2.PullRequest? { id.flatMap { pullRequests[$0] } }
    public func document(_ id: DomainV2.DocumentId?) -> DomainV2.Document? { id.flatMap { documents[$0] } }
    public func documentRevision(_ id: DomainV2.DocumentRevisionId?) -> DomainV2.DocumentRevision? { id.flatMap { revisions[$0] } }

    public func interaction(of item: DomainV2.InboxItem) -> DomainV2.Interaction? {
        guard case .interaction(let id) = item.subject else { return nil }
        return interactions[id]
    }

    public func tasks(in request: DomainV2.RequestId) -> [DomainV2.Task] {
        projection.tasks.filter { $0.requestId == request }
    }

    public func tasks(assignedTo agent: DomainV2.AgentId) -> [DomainV2.Task] {
        projection.tasks.filter { $0.agentId == agent }
    }

    public func dependencies(of task: DomainV2.Task) -> [DomainV2.Task] {
        task.dependencies.compactMap { tasks[$0] }
    }

    public func dependents(of task: DomainV2.TaskId) -> [DomainV2.Task] {
        projection.tasks.filter { $0.dependencies.contains(task) }
    }

    /// Inbox entries the server attributed to this Task, roots and children alike.
    public func inboxItems(forTask task: DomainV2.TaskId) -> [DomainV2.InboxItem] {
        func flatten(_ items: [DomainV2.InboxItem]) -> [DomainV2.InboxItem] {
            items.flatMap { [$0] + flatten($0.children) }
        }
        return flatten(roots).filter { $0.taskId == task }
    }

    public func documents(of task: DomainV2.Task) -> [TaskDocument] {
        task.documents.keys.sorted().compactMap { key in
            let id = DomainV2.DocumentId(rawValue: key)
            guard let document = documents[id] else { return nil }
            return TaskDocument(document: document, role: task.documents[key] ?? "",
                                currentRevision: revisions[document.currentRevision],
                                review: projection.documentReviews.first { $0.taskId == task.id && $0.documentId == id })
        }
    }

    public func mergeAuthorizations(for pr: DomainV2.PrId) -> [DomainV2.MergeAuthorization] {
        projection.mergeAuthorizations.filter { $0.prId == pr }
    }

    public func stackReadiness(for task: DomainV2.TaskId) -> [DomainV2.StackReadiness] {
        projection.stackReadiness.filter { $0.taskId == task }
    }
}

public struct TaskDocument: Equatable, Sendable, Identifiable {
    public var id: DomainV2.DocumentId { document.id }
    public let document: DomainV2.Document
    public let role: String
    public let currentRevision: DomainV2.DocumentRevision?
    public let review: DomainV2.DocumentReview?
}
