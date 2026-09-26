import AlondraContracts

extension Node {
    /// The main thread's tool in flight: all an alondrad without the activity ledger says.
    public var toolThread: [NodeActivity] {
        guard let tool else { return [] }
        return [NodeActivity(agentId: nil, agentType: nil, task: nil, tool: tool.name, summary: tool.summary, since: tool.since)]
    }
}

