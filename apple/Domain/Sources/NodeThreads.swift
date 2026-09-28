import AlondraContracts

extension Node {
    /// The main thread's tool in flight: all an alondrad without the activity ledger says.
    public var toolThread: [NodeActivity] {
        guard let tool else { return [] }
        return [NodeActivity(agentId: nil, agentType: nil, task: nil, tool: tool.name, summary: tool.summary, since: tool.since)]
    }

    public var occupancy: NodeOccupancy { NodeOccupancy(self) }
}

/// An Agent's technical occupancy from its runtime telemetry; never a Task's state.
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
