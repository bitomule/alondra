import Foundation

/// What the user asked for, in their words. Its state comes from its tasks.
public struct WorkRequest: Codable, Identifiable, Hashable, Sendable {
    public enum State: String, Codable, Hashable, Sendable {
        case defining
        case inProgress = "in_progress"
        case done

        public var label: String {
            switch self {
            case .defining: String(localized: "defining", table: "AlondraShared", bundle: #bundle)
            case .inProgress: String(localized: "in progress", table: "AlondraShared", bundle: #bundle)
            case .done: String(localized: "request-state.done", defaultValue: "done", table: "AlondraShared", bundle: #bundle)
            }
        }

        public init(from decoder: Decoder) throws {
            self = State(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .inProgress
        }
    }

    public struct Options: Codable, Hashable, Sendable {
        public let project: String?
        public let autoMerge: Bool?
        public let effort: String?
        public let issue: String?
    }

    public let id: String
    public let text: String?
    public let attachments: [Attachment]?
    public let options: Options?
    public let createdAt: Int64
    public let synthetic: Bool?
    public let state: State
    public let tasks: [String]?
    /// The title the orchestrator gave it.
    public let title: String?
    /// When it left the Tasks list: every task of it is archived, or it finished without tasks
    /// and was archived itself.
    public let archivedAt: Int64?
}

/// One piece of work a request became, with its own state machine on the server.
public struct WorkTask: Codable, Identifiable, Hashable, Sendable {
    public enum State: String, Codable, Hashable, Sendable, CaseIterable {
        case waitingDependency = "waiting_dependency"
        case running
        case needsYou = "needs_you"
        case inReview = "in_review"
        case closing
        case done
        case cancelled

        public var label: String {
            switch self {
            case .waitingDependency: String(localized: "waiting for another task", table: "AlondraShared", bundle: #bundle)
            case .running: String(localized: "running", table: "AlondraShared", bundle: #bundle)
            case .needsYou: String(localized: "needs you", table: "AlondraShared", bundle: #bundle)
            case .inReview: String(localized: "in review", table: "AlondraShared", bundle: #bundle)
            case .closing: String(localized: "closing", table: "AlondraShared", bundle: #bundle)
            case .done: String(localized: "task-state.done", defaultValue: "done", table: "AlondraShared", bundle: #bundle)
            case .cancelled: String(localized: "cancelled", table: "AlondraShared", bundle: #bundle)
            }
        }

        public var isFinished: Bool { self == .done || self == .cancelled }

        /// A state this build does not know yet reads as `running`, so the board keeps decoding
        /// when the daemon learns a new one.
        public init(from decoder: Decoder) throws {
            self = State(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .running
        }
    }

    public struct Options: Codable, Hashable, Sendable {
        public let autoMerge: Bool?
        public let effort: String?
        public let project: String?
        public let issue: String?

        public var mergesAlone: Bool { autoMerge == true }
    }

    public struct Assignee: Codable, Hashable, Sendable {
        public let sessionId: String
        public let agentId: String?
    }

    public struct Blocker: Codable, Hashable, Sendable {
        public let kind: String
        public let question: UInt64
        public let since: Int64

        public var isAction: Bool { kind == "action" }
    }

    public struct Event: Codable, Hashable, Sendable {
        public let kind: String
        public let task: String?
        public let pr: UInt64?
        public let report: UInt64?
        public let question: UInt64?
        public let blocker: Blocker?
    }

    public struct Transition: Codable, Hashable, Sendable {
        public let from: State
        public let to: State
        public let event: Event
        public let at: Int64
    }

    public let id: String
    public let request: String
    public let title: String
    public let brief: String?
    public let dependsOn: [String]
    public let options: Options?
    public let assignee: Assignee?
    public let prs: [UInt64]
    public let documents: [UInt64]
    public let blocker: Blocker?
    public let notices: [UInt64]
    public let state: State
    public let stateSince: Int64
    public let createdAt: Int64
    public let finalReport: UInt64?
    public let history: [Transition]?
    /// Alondra merges its PR by itself once the checks pass.
    public let autoMergePending: Bool?
    /// When the user archived it, once it was finished.
    public let archivedAt: Int64?
    /// When anything last happened to it, its history included.
    public let lastChange: Int64?

    private enum CodingKeys: String, CodingKey {
        case id, request, title, brief, dependsOn, options, assignee, prs, documents, blocker, notices, state, stateSince, createdAt, finalReport, history,
            autoMergePending, archivedAt, lastChange
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        request = try c.decode(String.self, forKey: .request)
        title = try c.decode(String.self, forKey: .title)
        brief = try c.decodeIfPresent(String.self, forKey: .brief)
        dependsOn = try c.decodeIfPresent([String].self, forKey: .dependsOn) ?? []
        options = try c.decodeIfPresent(Options.self, forKey: .options)
        assignee = try c.decodeIfPresent(Assignee.self, forKey: .assignee)
        prs = try c.decodeIfPresent([UInt64].self, forKey: .prs) ?? []
        documents = try c.decodeIfPresent([UInt64].self, forKey: .documents) ?? []
        blocker = try c.decodeIfPresent(Blocker.self, forKey: .blocker)
        notices = try c.decodeIfPresent([UInt64].self, forKey: .notices) ?? []
        state = try c.decode(State.self, forKey: .state)
        stateSince = try c.decode(Int64.self, forKey: .stateSince)
        createdAt = try c.decodeIfPresent(Int64.self, forKey: .createdAt) ?? stateSince
        finalReport = try c.decodeIfPresent(UInt64.self, forKey: .finalReport)
        history = try c.decodeIfPresent([Transition].self, forKey: .history)
        autoMergePending = try c.decodeIfPresent(Bool.self, forKey: .autoMergePending)
        archivedAt = try c.decodeIfPresent(Int64.self, forKey: .archivedAt)
        lastChange = try c.decodeIfPresent(Int64.self, forKey: .lastChange)
    }

    public var mergesAlone: Bool { autoMergePending == true }

    public var isArchived: Bool { archivedAt != nil }

    /// Moves when its history does, also when its state stays: an older alondrad only says when
    /// the state changed.
    public var historyKey: Int64 { lastChange ?? stateSince }

    /// Every report the task carries: documents, notices and its final report.
    public var reports: [UInt64] { documents + notices + (finalReport.map { [$0] } ?? []) }
}

/// What a node or one of its subagents is doing right now.
public struct NodeActivity: Codable, Hashable, Sendable {
    public let agentId: String?
    public let agentType: String?
    public let task: String?
    public let tool: String?
    public let summary: String?
    public let since: Int64

    public var isMainThread: Bool { agentId == nil }

    /// "main" for the node's own thread, the subagent's type otherwise.
    public var thread: String {
        if isMainThread { return String(localized: "main", table: "AlondraShared", bundle: #bundle) }
        return agentType ?? String(localized: "subagent", table: "AlondraShared", bundle: #bundle)
    }

    /// "Bash bazel build //app", or whichever half is known.
    public var doing: String? {
        let text = [tool, summary].compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }.joined(separator: " ")
        return text.isEmpty ? nil : text
    }
}

/// A task as its own route gives it: with its history and, from a newer alondrad, its documents,
/// which the overview only has among the newest reports.
public struct TaskInFull: Decodable, Sendable {
    public let task: WorkTask
    public let documents: [Report]?

    private enum CodingKeys: String, CodingKey {
        case task
        case documents = "reports"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        task = try c.decodeIfPresent(WorkTask.self, forKey: .task) ?? WorkTask(from: decoder)
        documents = try c.decodeIfPresent([Report].self, forKey: .documents)
    }
}

/// What `GET /v1/nodes/{key}` says each thread of the node does now; the overview leaves it out.
public struct NodeThreads: Decodable, Sendable {
    private struct Detail: Decodable, Sendable {
        let activity: [NodeActivity]?
    }

    private let node: Detail

    /// Nil from an alondrad without the activity ledger.
    public var activity: [NodeActivity]? { node.activity }
}

/// Whether a node works on something right now; its tasks carry everything else.
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

    public var label: String {
        switch self {
        case .busy: String(localized: "busy", table: "AlondraShared", bundle: #bundle)
        case .free: String(localized: "free", table: "AlondraShared", bundle: #bundle)
        case .waitingMachine: String(localized: "waiting for the machine", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension Node {
    public var occupancy: NodeOccupancy { NodeOccupancy(self) }

    /// The orchestrator keeps its own states; every other node is free or busy.
    public var fleetLabel: String { orchestratorState?.label ?? occupancy.label }
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

    public var title: String {
        switch kind {
        case .defining: String(localized: "Defining", table: "AlondraShared", bundle: #bundle)
        case .task(.waitingDependency): String(localized: "Waiting for another task", table: "AlondraShared", bundle: #bundle)
        case .task(.running): String(localized: "Running", table: "AlondraShared", bundle: #bundle)
        case .task(.needsYou): String(localized: "Needs you", table: "AlondraShared", bundle: #bundle)
        case .task(.inReview): String(localized: "In review", table: "AlondraShared", bundle: #bundle)
        case .task(.closing): String(localized: "Closing", table: "AlondraShared", bundle: #bundle)
        case .task(.done): String(localized: "task-group.done", defaultValue: "Done", table: "AlondraShared", bundle: #bundle)
        case .task(.cancelled): String(localized: "Cancelled", table: "AlondraShared", bundle: #bundle)
        case .archived: String(localized: "Archived", table: "AlondraShared", bundle: #bundle)
        }
    }

    public var isFinished: Bool {
        switch kind {
        case .task(let state): state.isFinished
        case .archived: true
        case .defining: false
        }
    }

    public static let order: [Kind] = [
        .task(.needsYou), .task(.inReview), .task(.running), .task(.closing), .task(.waitingDependency), .defining, .task(.done), .task(.cancelled), .archived,
    ]
}

/// Requests and tasks as the overview sends them, indexed for the apps.
public struct TaskBoard: Sendable {
    public let requests: [WorkRequest]
    public let tasks: [String: WorkTask]
    /// Every request with all its tasks.
    public let trees: [RequestTree]
    /// The Tasks list: each request with the tasks not archived; a request with nothing left in
    /// it is not here.
    public let activeTrees: [RequestTree]
    /// The archived section: each request with its archived tasks, or archived without tasks.
    public let archivedTrees: [RequestTree]

    public static let empty = TaskBoard(requests: [], tasks: [])

    public init(requests: [WorkRequest], tasks: [WorkTask]) {
        let byID = Dictionary(tasks.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        self.requests = requests.sorted { ($0.createdAt, $0.id) > ($1.createdAt, $1.id) }
        self.tasks = byID
        let members = self.requests.map { request in
            let listed = (request.tasks ?? []).compactMap { byID[$0] }
            return (request, listed.isEmpty ? tasks.filter { $0.request == request.id } : listed)
        }
        trees = members.map { RequestTree(request: $0, rows: Self.tree($1)) }
        activeTrees = members.compactMap { request, all in
            let open = all.filter { !$0.isArchived }
            guard all.isEmpty ? request.archivedAt == nil : !open.isEmpty else { return nil }
            return RequestTree(request: request, rows: Self.tree(open))
        }
        archivedTrees = members.compactMap { request, all in
            let archived = all.filter(\.isArchived)
            guard all.isEmpty ? request.archivedAt != nil : !archived.isEmpty else { return nil }
            return RequestTree(request: request, rows: Self.tree(archived))
        }
    }

    public func task(_ id: String) -> WorkTask? { tasks[id] }

    public func request(_ id: String) -> WorkRequest? { requests.first { $0.id == id } }

    public func tree(for request: String) -> RequestTree? { trees.first { $0.request.id == request } }

    public var openTrees: [RequestTree] { activeTrees.filter { $0.request.state != .done } }
    public var doneTrees: [RequestTree] { activeTrees.filter { $0.request.state == .done } }

    /// Archived tasks, and requests archived without tasks.
    public var archivedCount: Int { archivedTrees.reduce(0) { $0 + max($1.rows.count, 1) } }

    /// Something finished is not archived yet: a done or cancelled task, or a request that
    /// finished without tasks.
    public var hasArchivable: Bool {
        tasks.values.contains { $0.state.isFinished && !$0.isArchived } || activeTrees.contains { $0.rows.isEmpty && $0.request.state == .done }
    }

    public func tasks(onNode session: String) -> [WorkTask] {
        tasks.values.filter { $0.assignee?.sessionId == session && !$0.isArchived }.sorted(by: Self.creationOrder)
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
        let defining = requests.filter { $0.state == .defining }
        let taskless = activeTrees.filter { $0.rows.isEmpty && $0.request.state == .done }.map(\.request)
        let all = tasks.values.sorted(by: Self.creationOrder)
        let active = all.filter { !$0.isArchived }
        return TaskStateGroup.order.compactMap { kind in
            let group: TaskStateGroup = switch kind {
            case .defining: TaskStateGroup(kind: kind, tasks: [], requests: defining)
            case .task(.done): TaskStateGroup(kind: kind, tasks: active.filter { $0.state == .done }, requests: taskless)
            case .task(let state): TaskStateGroup(kind: kind, tasks: active.filter { $0.state == state }, requests: [])
            case .archived: TaskStateGroup(kind: kind, tasks: all.filter(\.isArchived), requests: archivedTrees.filter(\.rows.isEmpty).map(\.request))
            }
            return group.count == 0 ? nil : group
        }
    }

    /// Open tasks and requests still being defined: what the Tasks section counts.
    public var openCount: Int {
        tasks.values.count(where: { !$0.state.isFinished }) + requests.count(where: { $0.state == .defining })
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
