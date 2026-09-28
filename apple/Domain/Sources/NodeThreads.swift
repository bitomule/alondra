import Foundation
import AlondraContracts

extension Node {
    /// The main thread's tool in flight: all an alondrad without the activity ledger says.
    public var toolThread: [NodeActivity] {
        guard let tool else { return [] }
        return [NodeActivity(agentId: nil, agentType: nil, task: nil, tool: tool.name, summary: tool.summary, since: tool.since)]
    }

    public var occupancy: NodeOccupancy { NodeOccupancy(self) }

    /// An Agent the projection lists but the runtime read does not describe (a Mac that does
    /// not serve telemetry yet): its identity and name only, with no claimed activity.
    public init?(agent: DomainV2.Agent) {
        let json: [String: Any] = [
            "sessionId": agent.id.rawValue, "shortId": String(agent.id.rawValue.prefix(8)), "name": agent.name,
            "isOrchestrator": false, "hasPlugin": false, "state": NodeState.unclear.rawValue,
            "stateSince": 0, "stateSource": "projection",
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: json),
              let node = try? JSONDecoder().decode(Node.self, from: data) else { return nil }
        self = node
    }
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
