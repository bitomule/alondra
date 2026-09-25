import AlondraShared
import Foundation

public struct FleetNode: Equatable, Sendable, Identifiable {
    public var id: String { node.sessionId }
    public let node: Node
    public let tasks: [WorkTask]
    public let pendingCount: Int

    public init(node: Node, tasks: [WorkTask], pendingCount: Int) {
        self.node = node
        self.tasks = tasks
        self.pendingCount = pendingCount
    }
}

public struct FleetSection: Equatable, Sendable, Identifiable {
    public var id: String { name }
    public let name: String
    public let nodes: [FleetNode]
}

/// An accepted sync snapshot. A failed refresh is represented separately as `snapshotUnavailable`.
public struct FleetSnapshot: Equatable, Sendable {
    /// Monotonic delivery sequence assigned by the process-owned sync projection, not daemon tick.
    public let sequence: UInt64
    public let tick: UInt64
    public let orchestrator: FleetNode?
    public let sections: [FleetSection]

    public init(sequence: UInt64, tick: UInt64, orchestrator: FleetNode?, sections: [FleetSection]) {
        self.sequence = sequence
        self.tick = tick
        self.orchestrator = orchestrator
        self.sections = sections
    }

    public func node(_ id: String) -> FleetNode? {
        if orchestrator?.id == id { return orchestrator }
        return sections.lazy.flatMap(\.nodes).first { $0.id == id }
    }
}

/// Keeps ordering and search rules out of both platform views and reducers.
public struct ProjectFleetUseCase: Sendable {
    public init() {}

    /// `visibleInboxIDs` comes from the same presentation owner as the inbox. Hidden/resolved
    /// cards must not be counted as pending even while an old server snapshot still contains them.
    public func execute(_ overview: Overview, visibleInboxIDs: Set<String>, sequence: UInt64) -> FleetSnapshot {
        let board = TaskBoard(requests: overview.requests ?? [], tasks: overview.tasks ?? [])
        let grouping = InboxGrouping(
            questions: overview.openQuestions.filter { $0.toOrchestrator != true },
            reports: overview.news.filter { $0.toOrchestrator != true },
            pullRequests: overview.prs ?? []
        )
        // A question or report attached to an open PR is already represented by that PR card.
        let pending = (grouping.loose.map(InboxItem.question)
            + grouping.looseReports.map(InboxItem.report)
            + (overview.prs ?? []).map(InboxItem.pullRequest))
            .filter { visibleInboxIDs.contains($0.id) }
            .map(\.sessionId)
        let counts = pending.reduce(into: [String: Int]()) { $0[$1, default: 0] += 1 }
        let project = { (node: Node) in
            FleetNode(node: node, tasks: node.isOrchestrator ? [] : board.tasks(onNode: node.sessionId),
                      pendingCount: counts[node.sessionId, default: 0])
        }
        let orchestrator = overview.nodes.filter(\.isOrchestrator)
            .max { $0.stateSince < $1.stateSince }.map(project)
        let groups = Dictionary(grouping: overview.nodes.filter { !$0.isOrchestrator }) {
            $0.projectName ?? Node.otherProjectName
        }
        let sections = groups.map { name, nodes in
            FleetSection(name: name, nodes: nodes.sorted {
                ($0.createdAt, $0.sessionId) < ($1.createdAt, $1.sessionId)
            }.map(project))
        }.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        return FleetSnapshot(sequence: sequence, tick: overview.tick, orchestrator: orchestrator, sections: sections)
    }

    public func search(_ snapshot: FleetSnapshot, query: String) -> [FleetSection] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return snapshot.sections }
        return snapshot.sections.compactMap { section in
            let nodes = section.nodes.filter { entry in
                let node = entry.node
                let terms = [node.displayName, section.name, node.summary, node.cwd ?? "", node.fleetLabel]
                    + entry.tasks.map(\.title)
                return terms.contains { $0.localizedStandardContains(needle) }
            }
            return nodes.isEmpty ? nil : FleetSection(name: section.name, nodes: nodes)
        }
    }
}
