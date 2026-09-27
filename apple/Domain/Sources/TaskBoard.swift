import AlondraContracts
import Foundation

public enum NodeOccupancy: Hashable, Sendable {
    case busy
    case free
    case waitingMachine

    public init(_ node: Node) {
        switch node.state {
        case .working: self = .busy
        case .waitingMachine: self = .waitingMachine
        default: self = .free
        }
    }

}

extension Node {
    public var occupancy: NodeOccupancy { NodeOccupancy(self) }

}

/// A task placed in its request's tree: `depth` 0 runs in parallel with its siblings, a deeper
/// task goes after its `parent`.
public struct TaskTreeRow: Identifiable, Hashable, Sendable {
    public let task: WorkTask
    public let depth: Int
    public let parent: String?
    public let hasChildren: Bool
    public let isLastSibling: Bool
    /// For each level above this row's parent, whether that ancestor still has siblings below,
    /// so its line runs through this row.
    public let linesThrough: [Bool]

    public var id: String { task.id }
}

public struct RequestTree: Identifiable, Hashable, Sendable {
    public let request: WorkRequest
    public let rows: [TaskTreeRow]

    public var id: String { request.id }
    public var tasks: [WorkTask] { rows.map(\.task) }

    /// Every task of it is done or cancelled, or it ended without tasks.
    public var isFinished: Bool { rows.isEmpty ? request.state == .done : rows.allSatisfy { $0.task.state.isFinished } }

    /// Every task of it was cancelled, or the user withdrew it.
    public var isCancelled: Bool { rows.isEmpty ? request.closed?.withdrawn == true : rows.allSatisfy { $0.task.state == .cancelled } }

    /// When it last moved, which orders the list: the newest change of its tasks, else when it
    /// was closed or sent.
    public var changedAt: Int64 { rows.map(\.task.changedAt).max() ?? request.closed?.at ?? request.createdAt }
}

extension WorkTask.State {
    /// The state the user sees: not started, running, waiting for them (a question, an action or
    /// a PR to review), finished or cancelled. The steps in between are Alondra's, not theirs.
    public var shown: WorkTask.State {
        switch self {
        case .notStarted: .notStarted
        case .waitingDependency, .running, .closing: .running
        case .needsYou, .inReview: .needsYou
        case .done: .done
        case .cancelled: .cancelled
        }
    }
}

/// The tasks grouped the way the Mac sidebar lists them: what waits for the user first.
public struct TaskStateGroup: Identifiable, Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case task(WorkTask.State)
        case defining
        /// Finished work the user archived, and requests archived without tasks.
        case archived
    }

    public let kind: Kind
    public let tasks: [WorkTask]
    public let requests: [WorkRequest]

    public init(kind: Kind, tasks: [WorkTask], requests: [WorkRequest]) {
        self.kind = kind
        self.tasks = tasks
        self.requests = requests
    }

    public var id: Kind { kind }
    public var count: Int { tasks.count + requests.count }

    public var isFinished: Bool {
        switch kind {
        case .task(let state): state.isFinished
        case .archived: true
        case .defining: false
        }
    }

    public static let order: [Kind] = [
        .task(.needsYou), .task(.running), .task(.notStarted), .defining, .task(.done), .task(.cancelled), .archived,
    ]
}

/// Requests and tasks as the overview sends them, indexed for the apps. Probe sessions are left
/// out of every list and count (`isHidden`), as in the TUI.
public struct TaskBoard: Sendable {
    public let requests: [WorkRequest]
    public let tasks: [String: WorkTask]
    /// The tasks the lists leave out.
    public let hidden: Set<String>
    /// Every request with all its tasks.
    public let trees: [RequestTree]
    /// The Tasks list: each request with the tasks not archived, what is open first, the one
    /// that changed last first, then what finished, the last to finish first; a request with
    /// nothing left in it is not here.
    public let activeTrees: [RequestTree]
    /// The archived section: each request with its archived tasks, or archived without tasks.
    public let archivedTrees: [RequestTree]

    public static let empty = TaskBoard(requests: [], tasks: [])

    public init(requests: [WorkRequest], tasks: [WorkTask]) {
        let byID = Dictionary(tasks.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        let requestsByID = Dictionary(requests.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        let hidden = Set(tasks.filter { Self.isProbe($0, in: requestsByID[$0.request]) }.map(\.id))
        self.requests = requests.sorted { ($0.createdAt, $0.id) > ($1.createdAt, $1.id) }
        self.tasks = byID
        self.hidden = hidden
        let members: [(WorkRequest, [WorkTask])] = self.requests.compactMap { request in
            let listed = (request.tasks ?? []).compactMap { byID[$0] }
            let all = listed.isEmpty ? tasks.filter { $0.request == request.id } : listed
            let shown = all.filter { !hidden.contains($0.id) }
            guard all.isEmpty ? request.synthetic != true : !shown.isEmpty else { return nil }
            return (request, shown)
        }
        trees = members.map { RequestTree(request: $0, rows: Self.tree($1)) }
        let active = members.compactMap { request, all -> RequestTree? in
            let open = all.filter { !$0.isArchived }
            guard all.isEmpty ? request.archivedAt == nil : !open.isEmpty else { return nil }
            return RequestTree(request: request, rows: Self.tree(open))
        }
        let byChange = { (a: RequestTree, b: RequestTree) in (a.changedAt, a.id) > (b.changedAt, b.id) }
        activeTrees = active.filter { !$0.isFinished }.sorted(by: byChange) + active.filter(\.isFinished).sorted(by: byChange)
        archivedTrees = members.compactMap { request, all in
            let archived = all.filter(\.isArchived)
            guard all.isEmpty ? request.archivedAt != nil : !archived.isEmpty else { return nil }
            return RequestTree(request: request, rows: Self.tree(archived))
        }
    }

    /// A session no request of the user's stands behind that was a test node or that ended
    /// cancelled without delivering anything: a node's own task, never recorded, on a request
    /// made up for it.
    static func isProbe(_ task: WorkTask, in request: WorkRequest?) -> Bool {
        guard !task.recorded, request?.synthetic == true else { return false }
        return task.title.hasPrefix(probePrefix) || (task.state == .cancelled && !task.delivered)
    }

    /// The daemon's name prefix for test nodes.
    static let probePrefix = "test/"

    public func isHidden(_ task: WorkTask) -> Bool { hidden.contains(task.id) }

    public func task(_ id: String) -> WorkTask? { tasks[id] }

    public func request(_ id: String) -> WorkRequest? { requests.first { $0.id == id } }

    public func tree(for request: String) -> RequestTree? { trees.first { $0.request.id == request } }

    public var openTrees: [RequestTree] { activeTrees.filter { !$0.isFinished } }
    public var doneTrees: [RequestTree] { activeTrees.filter(\.isFinished) }

    /// A task's title on its row: the orchestrator's title for its request when it is that
    /// request's only task, else its own.
    public func rowTitle(of task: WorkTask) -> String {
        guard let request = request(task.request), tree(for: request.id)?.rows.count == 1,
            let title = request.title?.trimmingCharacters(in: .whitespacesAndNewlines), !title.isEmpty
        else { return task.title }
        return title
    }

    /// Archived tasks, and requests archived without tasks.
    public var archivedCount: Int { archivedTrees.reduce(0) { $0 + max($1.rows.count, 1) } }

    /// Something finished is not archived yet: a done or cancelled task, or a request that
    /// finished without tasks.
    public var hasArchivable: Bool {
        tasks.values.contains { $0.state.isFinished && !$0.isArchived } || activeTrees.contains { $0.rows.isEmpty && $0.request.state == .done }
    }

    public func tasks(onNode session: String) -> [WorkTask] {
        tasks.values.filter { $0.assignee?.sessionId == session && !$0.isArchived && !isHidden($0) }.sorted(by: Self.creationOrder)
    }

    public func dependencies(of task: WorkTask) -> [WorkTask] {
        task.dependsOn.compactMap { tasks[$0] }
    }

    public func dependents(of task: WorkTask) -> [WorkTask] {
        tasks.values.filter { $0.dependsOn.contains(task.id) }.sorted(by: Self.creationOrder)
    }

    /// The task, everything it builds on and everything built on it, in the order their PRs merge.
    public func mergeOrder(around task: WorkTask) -> [WorkTask] {
        var below: [WorkTask] = []
        var seen: Set<String> = [task.id]
        func down(_ t: WorkTask) {
            for dep in dependencies(of: t) where seen.insert(dep.id).inserted {
                down(dep)
                below.append(dep)
            }
        }
        down(task)
        var above: [WorkTask] = []
        var frontier = [task]
        while !frontier.isEmpty {
            let next = frontier.flatMap(dependents(of:)).filter { seen.insert($0.id).inserted }
            above.append(contentsOf: next)
            frontier = next
        }
        return below + [task] + above
    }

    public var groups: [TaskStateGroup] {
        let defining = requests.filter { $0.state == .defining && $0.synthetic != true }
        let taskless = activeTrees.filter { $0.rows.isEmpty && $0.request.state == .done }.map(\.request)
        let all = tasks.values.filter { !isHidden($0) }.sorted(by: Self.creationOrder)
        let active = all.filter { !$0.isArchived }
        return TaskStateGroup.order.compactMap { kind in
            let group: TaskStateGroup = switch kind {
            case .defining: TaskStateGroup(kind: kind, tasks: [], requests: defining)
            case .task(.done): TaskStateGroup(kind: kind, tasks: active.filter { $0.state == .done }, requests: taskless)
            case .task(let state): TaskStateGroup(kind: kind, tasks: active.filter { $0.state.shown == state }, requests: [])
            case .archived: TaskStateGroup(kind: kind, tasks: all.filter(\.isArchived), requests: archivedTrees.filter(\.rows.isEmpty).map(\.request))
            }
            return group.count == 0 ? nil : group
        }
    }

    /// Open tasks and requests still being defined, probe sessions left out: what the Tasks
    /// section counts.
    public var openCount: Int {
        tasks.values.count(where: { !$0.state.isFinished && !isHidden($0) }) + requests.count(where: { $0.state == .defining && $0.synthetic != true })
    }

    /// The task a card belongs to: the one it names, else the one that carries it.
    public func task(for item: InboxItem) -> WorkTask? {
        switch item {
        case .question(let q):
            if let id = q.task { return tasks[id] }
            return tasks.values.first { $0.blocker?.question == q.id }
        case .report(let r):
            if let id = r.task { return tasks[id] }
            return tasks.values.first { $0.reports.contains(r.id) }
        case .pullRequest(let p):
            if let id = p.task { return tasks[id] }
            return tasks.values.first { $0.prs.contains(p.id) }
        }
    }

    static func creationOrder(_ a: WorkTask, _ b: WorkTask) -> Bool {
        (a.createdAt, a.id) < (b.createdAt, b.id)
    }

    /// Each task under the dependency it goes after (the newest one when it has several);
    /// tasks with no dependency in the request are roots, in parallel.
    static func tree(_ members: [WorkTask]) -> [TaskTreeRow] {
        let ordered = members.sorted(by: creationOrder)
        let ids = Set(ordered.map(\.id))
        let byID = Dictionary(ordered.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        var parent: [String: String] = [:]
        for task in ordered {
            let inside = task.dependsOn.filter { ids.contains($0) && $0 != task.id }.compactMap { byID[$0] }
            if let last = inside.max(by: creationOrder) { parent[task.id] = last.id }
        }
        var children: [String: [WorkTask]] = [:]
        for task in ordered {
            if let p = parent[task.id] { children[p, default: []].append(task) }
        }
        var rows: [TaskTreeRow] = []
        var placed: Set<String> = []
        func place(_ task: WorkTask, depth: Int, isLast: Bool, lines: [Bool]) {
            guard placed.insert(task.id).inserted else { return }
            let kids = (children[task.id] ?? []).filter { !placed.contains($0.id) }
            rows.append(TaskTreeRow(task: task, depth: depth, parent: parent[task.id], hasChildren: !kids.isEmpty, isLastSibling: isLast, linesThrough: lines))
            let below = depth == 0 ? lines : lines + [!isLast]
            for (i, kid) in kids.enumerated() {
                place(kid, depth: depth + 1, isLast: i == kids.count - 1, lines: below)
            }
        }
        let roots = ordered.filter { parent[$0.id] == nil }
        for (i, root) in roots.enumerated() {
            place(root, depth: 0, isLast: i == roots.count - 1, lines: [])
        }
        for task in ordered where !placed.contains(task.id) {
            place(task, depth: 0, isLast: true, lines: [])
        }
        return rows
    }
}
