import AlondraContracts
import Foundation

extension DomainV2.TaskState {
    public var isFinished: Bool { self == .done || self == .cancelled }
}

/// A task placed in its request's tree from its explicit dependencies: `depth` 0 runs in
/// parallel with its siblings, a deeper task goes after its `parent`.
public struct TaskTreeRow: Identifiable, Hashable, Sendable {
    public let task: DomainV2.Task
    public let depth: Int
    public let parent: DomainV2.TaskId?
    public let hasChildren: Bool
    public let isLastSibling: Bool
    /// For each level above this row's parent, whether that ancestor still has siblings below,
    /// so its line runs through this row.
    public let linesThrough: [Bool]

    public var id: DomainV2.TaskId { task.id }
}

public struct RequestTree: Identifiable, Hashable, Sendable {
    public let request: DomainV2.Request
    public let rows: [TaskTreeRow]

    public var id: DomainV2.RequestId { request.id }
    public var tasks: [DomainV2.Task] { rows.map(\.task) }

    /// Every task of it is done or cancelled, or it was closed without tasks.
    public var isFinished: Bool { rows.isEmpty ? request.closed : rows.allSatisfy { $0.task.state.isFinished } }
}

/// Tasks grouped by the exact state the server gives them; no two states are merged.
public struct TaskStateGroup: Identifiable, Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case task(DomainV2.TaskState)
        case archived
    }

    public let kind: Kind
    public let tasks: [DomainV2.Task]

    public init(kind: Kind, tasks: [DomainV2.Task]) {
        self.kind = kind
        self.tasks = tasks
    }

    public var id: Kind { kind }
    public var count: Int { tasks.count }

    public var isFinished: Bool {
        switch kind {
        case .task(let state): state.isFinished
        case .archived: true
        }
    }

    public static let order: [Kind] = [
        .task(.needsYou), .task(.inReview), .task(.closing), .task(.running), .task(.waitingDependency),
        .task(.notStarted), .task(.done), .task(.cancelled), .archived,
    ]
}

/// Requests and tasks as the projection sends them, in the server's order.
public struct TaskBoard: Equatable, Sendable {
    public let requests: [DomainV2.Request]
    public let tasks: [DomainV2.TaskId: DomainV2.Task]
    private let ordered: [DomainV2.Task]
    /// Every request with all its tasks.
    public let trees: [RequestTree]
    /// Each request with its tasks that are not archived, open requests first.
    public let activeTrees: [RequestTree]
    /// Each request with its archived tasks.
    public let archivedTrees: [RequestTree]

    public static let empty = TaskBoard(requests: [], tasks: [])

    public init(_ workspace: DomainWorkspace) {
        self.init(requests: workspace.projection.requests, tasks: workspace.projection.tasks)
    }

    public init(requests: [DomainV2.Request], tasks: [DomainV2.Task]) {
        self.requests = requests
        ordered = tasks
        self.tasks = Dictionary(tasks.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let members = requests.map { request in (request, tasks.filter { $0.requestId == request.id }) }
        trees = members.map { RequestTree(request: $0, rows: Self.tree($1)) }
        let active = members.compactMap { request, all -> RequestTree? in
            let open = all.filter { !$0.archived }
            guard all.isEmpty ? !request.closed : !open.isEmpty else { return nil }
            return RequestTree(request: request, rows: Self.tree(open))
        }
        activeTrees = active.filter { !$0.isFinished } + active.filter(\.isFinished)
        archivedTrees = members.compactMap { request, all in
            let archived = all.filter(\.archived)
            guard !archived.isEmpty else { return nil }
            return RequestTree(request: request, rows: Self.tree(archived))
        }
    }

    public func task(_ id: DomainV2.TaskId) -> DomainV2.Task? { tasks[id] }

    public func request(_ id: DomainV2.RequestId) -> DomainV2.Request? { requests.first { $0.id == id } }

    public func tree(for request: DomainV2.RequestId) -> RequestTree? { trees.first { $0.request.id == request } }

    public var openTrees: [RequestTree] { activeTrees.filter { !$0.isFinished } }
    public var doneTrees: [RequestTree] { activeTrees.filter(\.isFinished) }

    public var archivedCount: Int { archivedTrees.reduce(0) { $0 + $1.rows.count } }

    /// An Agent may hold several Tasks; finishing one leaves the others as they are.
    public func tasks(assignedTo agent: DomainV2.AgentId) -> [DomainV2.Task] {
        ordered.filter { $0.agentId == agent && !$0.archived }
    }

    public func dependencies(of task: DomainV2.Task) -> [DomainV2.Task] {
        task.dependencies.compactMap { tasks[$0] }
    }

    public func dependents(of task: DomainV2.Task) -> [DomainV2.Task] {
        ordered.filter { $0.dependencies.contains(task.id) }
    }

    public var groups: [TaskStateGroup] {
        let active = ordered.filter { !$0.archived }
        return TaskStateGroup.order.compactMap { kind in
            let group: TaskStateGroup = switch kind {
            case .task(let state): TaskStateGroup(kind: kind, tasks: active.filter { $0.state == state })
            case .archived: TaskStateGroup(kind: kind, tasks: ordered.filter(\.archived))
            }
            return group.count == 0 ? nil : group
        }
    }

    public var openCount: Int { ordered.count { !$0.state.isFinished && !$0.archived } }

    /// Each task under the dependency it goes after (the last listed when it has several);
    /// tasks with no dependency in the request are roots, in parallel.
    static func tree(_ members: [DomainV2.Task]) -> [TaskTreeRow] {
        let ids = Set(members.map(\.id))
        let position = Dictionary(members.enumerated().map { ($0.element.id, $0.offset) }, uniquingKeysWith: { first, _ in first })
        var parent: [DomainV2.TaskId: DomainV2.TaskId] = [:]
        for task in members {
            let inside = task.dependencies.filter { ids.contains($0) && $0 != task.id }
            if let last = inside.max(by: { (position[$0] ?? 0) < (position[$1] ?? 0) }) { parent[task.id] = last }
        }
        var children: [DomainV2.TaskId: [DomainV2.Task]] = [:]
        for task in members {
            if let p = parent[task.id] { children[p, default: []].append(task) }
        }
        var rows: [TaskTreeRow] = []
        var placed: Set<DomainV2.TaskId> = []
        func place(_ task: DomainV2.Task, depth: Int, isLast: Bool, lines: [Bool]) {
            guard placed.insert(task.id).inserted else { return }
            let kids = (children[task.id] ?? []).filter { !placed.contains($0.id) }
            rows.append(TaskTreeRow(task: task, depth: depth, parent: parent[task.id], hasChildren: !kids.isEmpty,
                                    isLastSibling: isLast, linesThrough: lines))
            let below = depth == 0 ? lines : lines + [!isLast]
            for (i, kid) in kids.enumerated() {
                place(kid, depth: depth + 1, isLast: i == kids.count - 1, lines: below)
            }
        }
        let roots = members.filter { parent[$0.id] == nil }
        for (i, root) in roots.enumerated() {
            place(root, depth: 0, isLast: i == roots.count - 1, lines: [])
        }
        for task in members where !placed.contains(task.id) {
            place(task, depth: 0, isLast: true, lines: [])
        }
        return rows
    }
}
