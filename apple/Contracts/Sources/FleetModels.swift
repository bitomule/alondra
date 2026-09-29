import Foundation

public enum NodeState: String, Codable, Sendable {
    case working
    case waitingAnswer = "waiting_answer"
    case yourTurn = "your_turn"
    case waitingMerge = "waiting_merge"
    case delivered
    case waitingMachine = "waiting_machine"
    case done
    case unclear
    case stalled

    /// A state this build does not know yet reads as `unclear`, so the fleet keeps decoding when
    /// the daemon learns a new one.
    public init(from decoder: Decoder) throws {
        self = NodeState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unclear
    }
}

public enum OrchestratorState: String, Codable, Sendable {
    case onDuty = "on_duty"
    case handling
    case waitingForYou = "waiting_for_you"
    case offline
    case unknown

    public init(from decoder: Decoder) throws {
        self = OrchestratorState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// Public lifecycle of the managed orchestrator, independent of a running Claude process.
public struct OrchestratorRuntime: Codable, Hashable, Sendable {
    public enum Phase: String, Codable, Sendable {
        case idle, starting, running, compacting, failed, unknown
        public init(from decoder: Decoder) throws {
            self = Phase(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
        }
    }
    public let phase: Phase
    public let canRetry: Bool?
    public let canCancel: Bool?
    public let cancelRequested: Bool?
    public let activeRunId: String?
    public let queueDepth: Int?
    public let lastError: String?
    public let retryAt: Int64?
    public let updatedAt: Int64?
}

public struct ToolInFlight: Codable, Hashable, Sendable {
    public let name: String
    public let summary: String?
    public let since: Int64
}

public struct Node: Codable, Identifiable, Hashable, Sendable {
    public var id: String { sessionId }
    public let sessionId: String
    public let shortId: String
    public let name: String?
    public let cwd: String?
    public let isOrchestrator: Bool
    public let hasPlugin: Bool
    public let state: NodeState
    public let orchestratorState: OrchestratorState?
    public let orchestratorRuntime: OrchestratorRuntime?
    /// When the node first appeared; fixed for its lifetime. The fleet list sorts by this, not
    /// by state, so a node keeps its place as its state changes.
    public let createdAt: Int64
    public let stateSince: Int64
    public let stateSource: String
    public let tool: ToolInFlight?
    public let lastMessage: String?
    public let lastMessageAt: Int64?
    public let daemonDetail: String?
    public let daemonNeeds: String?
    public let project: String?
    public let background: [BackgroundItem]?
    /// What each of its threads does now: the main one and every subagent. An older alondrad
    /// sends none.
    public let activity: [NodeActivity]?

    private enum CodingKeys: String, CodingKey {
        case sessionId, shortId, name, cwd, isOrchestrator, hasPlugin, state, orchestratorState, orchestratorRuntime, createdAt, stateSince, stateSource, tool,
            lastMessage, lastMessageAt, daemonDetail, daemonNeeds, project, background, activity
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try c.decode(String.self, forKey: .sessionId)
        shortId = try c.decode(String.self, forKey: .shortId)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        cwd = try c.decodeIfPresent(String.self, forKey: .cwd)
        isOrchestrator = try c.decode(Bool.self, forKey: .isOrchestrator)
        hasPlugin = try c.decode(Bool.self, forKey: .hasPlugin)
        state = try c.decode(NodeState.self, forKey: .state)
        orchestratorState = try c.decodeIfPresent(OrchestratorState.self, forKey: .orchestratorState)
        orchestratorRuntime = try c.decodeIfPresent(OrchestratorRuntime.self, forKey: .orchestratorRuntime)
        stateSince = try c.decode(Int64.self, forKey: .stateSince)
        // A node from a server older than this field falls back to `stateSince`, the closest
        // thing it has to a creation time — the same backfill the daemon runs on its own store.
        createdAt = try c.decodeIfPresent(Int64.self, forKey: .createdAt) ?? stateSince
        stateSource = try c.decode(String.self, forKey: .stateSource)
        tool = try c.decodeIfPresent(ToolInFlight.self, forKey: .tool)
        lastMessage = try c.decodeIfPresent(String.self, forKey: .lastMessage)
        lastMessageAt = try c.decodeIfPresent(Int64.self, forKey: .lastMessageAt)
        daemonDetail = try c.decodeIfPresent(String.self, forKey: .daemonDetail)
        daemonNeeds = try c.decodeIfPresent(String.self, forKey: .daemonNeeds)
        project = try c.decodeIfPresent(String.self, forKey: .project)
        background = try c.decodeIfPresent([BackgroundItem].self, forKey: .background)
        activity = try c.decodeIfPresent([NodeActivity].self, forKey: .activity)
    }

    public var running: [BackgroundItem] { background ?? [] }

    public var displayName: String { name ?? shortId }

    public var summary: String {
        if let tool { return "\(tool.name) \(tool.summary ?? "")" }
        return lastMessage ?? daemonNeeds ?? daemonDetail ?? ""
    }
}

public struct Machine: Codable, Hashable, Sendable {
    public let memoryTotal: UInt64
    public let memoryAvailable: UInt64
    public let memoryFreePercent: UInt32?
    public let load: [Double]
    public let cpus: Int?
    public let cpuUsage: Double?
    public let diskAvailable: UInt64
    public let unreliable: Bool
}

public struct Usage: Codable, Hashable, Sendable {
    public let weekPercent: Double
    public let weekResetsAt: Int64
    public let todaySpent: Double?
    public let todayShare: Double?
    public let perDayLeft: Double
    public let overPace: Bool?

    public var isOverPace: Bool { overPace == true }

}

public struct SearchResult: Codable, Sendable {
    public let via: String
    public let sessions: [String]
}
