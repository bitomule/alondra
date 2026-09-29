import AlondraContracts
import AlondraDomain
import AlondraShared
import Foundation

/// Keeps ordering and search rules out of both platform views and reducers.
public struct ProjectFleetUseCase: Sendable {
    public init() {}

    /// Agents come from the projection; their telemetry from the runtime read. Pending counts
    /// are the server's pending inbox roots attributed to each Agent and still visible here.
    public func execute(_ workspace: DomainWorkspace, visibleInboxIDs: Set<String>, sequence: UInt64) -> FleetSnapshot {
        let board = TaskBoard(workspace)
        let pending = workspace.roots.filter { $0.pending && visibleInboxIDs.contains($0.id) }.compactMap(\.agentId?.rawValue)
        let counts = pending.reduce(into: [String: Int]()) { $0[$1, default: 0] += 1 }
        let runtime = workspace.runtime?.nodes ?? []
        let known = Set(runtime.map(\.sessionId))
        let nodes = runtime + workspace.projection.agents.filter { !known.contains($0.id.rawValue) }.compactMap(Node.init(agent:))
        let project = { (node: Node) in
            FleetNode(node: node, tasks: node.isOrchestrator ? [] : board.tasks(assignedTo: DomainV2.AgentId(rawValue: node.sessionId)),
                      pendingCount: counts[node.sessionId, default: 0])
        }
        let orchestrator = nodes.filter(\.isOrchestrator)
            .max { $0.stateSince < $1.stateSince }.map(project)
        let groups = Dictionary(grouping: nodes.filter { !$0.isOrchestrator }) {
            $0.projectName ?? Node.otherProjectName
        }
        let sections = groups.map { name, nodes in
            FleetSection(name: name, nodes: nodes.sorted {
                ($0.createdAt, $0.sessionId) < ($1.createdAt, $1.sessionId)
            }.map(project))
        }.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        return FleetSnapshot(sequence: sequence, tick: workspace.revision, orchestrator: orchestrator, sections: sections)
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
