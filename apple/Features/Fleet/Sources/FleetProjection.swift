import AlondraDomain
import AlondraShared
import Foundation

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
