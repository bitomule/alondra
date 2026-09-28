import AlondraContracts
import Foundation

public struct FleetNode: Equatable, Sendable, Identifiable {
    public var id: String { node.sessionId }
    public let node: Node
    public let tasks: [DomainV2.Task]
    public let pendingCount: Int

    public init(node: Node, tasks: [DomainV2.Task], pendingCount: Int) {
        self.node = node
        self.tasks = tasks
        self.pendingCount = pendingCount
    }
}

public struct FleetSection: Equatable, Sendable, Identifiable {
    public var id: String { name }
    public let name: String
    public let nodes: [FleetNode]

    public init(name: String, nodes: [FleetNode]) {
        self.name = name
        self.nodes = nodes
    }
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
