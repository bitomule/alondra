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

public struct QuestionOption: Codable, Hashable, Sendable {
    public let label: String
    public let description: String?
}

public struct LinkRef: Codable, Hashable, Sendable {
    public let url: String
    public let title: String?
}

public struct Attachment: Codable, Hashable, Sendable {
    public let name: String
    public let sourcePath: String
    public let storedPath: String?
}

public struct Question: Codable, Identifiable, Hashable, Sendable {
    public let id: UInt64
    public let sessionId: String
    public let nodeName: String?
    public let title: String
    public let detail: String?
    public let options: [QuestionOption]
    public let links: [LinkRef]
    public let attachments: [Attachment]
    public let origin: String
    /// `menu:<short>` identifies a terminal menu. Its answer accepts only an exact option.
    public let toolUseId: String?
    public let nodeContext: String?
    public let status: String
    public let askedAt: Int64
    public let action: Bool?
    public let steps: [String]?
    public let mission: String?
    /// A handoff for the orchestrator, not a decision for the user; the server already keeps
    /// these out of `open_questions`, this is a defensive client-side guard.
    public let toOrchestrator: Bool?
    /// The open PR card this question belongs to; it is shown inside that card.
    public let prId: UInt64?
    /// The report this question is about; the server keeps it out of the inbox and it is shown
    /// inside the question instead.
    public let report: Report?
    /// Every report the question takes in, oldest first: its task's unread reports outside its PR
    /// card and its review cards. An older alondrad sends only `report`.
    public let reports: [Report]?
    /// The task this question blocks.
    public let task: String?

    public var aboutReports: [Report] { reports ?? report.map { [$0] } ?? [] }

    /// Something to do outside Alondra: answered with "Done" or with why it cannot be done.
    public var isAction: Bool { action == true }

    public var isTerminalMenu: Bool { toolUseId?.hasPrefix("menu:") == true }

    /// The steps numbered ahead of the detail, so every client reads an action the same way.
    public var readableDetail: String? {
        let numbered = (steps ?? []).enumerated().map { "\($0.offset + 1). \($0.element)" }.joined(separator: "\n")
        let parts = [numbered, detail ?? ""].filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: "\n\n")
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

public struct Report: Codable, Identifiable, Hashable, Sendable {
    public let id: UInt64
    public let sessionId: String
    public let nodeName: String?
    public let title: String
    public let body: String?
    public let links: [LinkRef]
    public let attachments: [Attachment]
    public let kind: String
    public let createdAt: Int64
    public let mission: String?
    /// The server's report gate thought it asks for something; it is delivered anyway, marked.
    public let looksLikeQuestion: Bool?
    /// A handoff for the orchestrator, not a result for the user; the server already keeps
    /// these out of `news`, this is a defensive client-side guard.
    public let toOrchestrator: Bool?
    /// The open PR card this report is about; it is shown inside that card.
    public let prId: UInt64?
    /// The other unread reports of its task that no question carries, oldest first; this card
    /// carries them and reading it reads them.
    public let earlier: [Report]?
    /// The task this report belongs to.
    public let task: String?

    public var seemsToAsk: Bool { looksLikeQuestion == true }

}

public struct SentMessage: Codable, Identifiable, Hashable, Sendable {
    public let id: UInt64
    public let from: String
    public let text: String
    public let createdAt: Int64
    public let handedAt: Int64?
    public let deliveredAt: Int64?
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

public struct Overview: Codable, Sendable {
    public let tick: UInt64
    public let nodes: [Node]
    public let openQuestions: [Question]
    public let news: [Report]
    public let orchestratorMessages: [SentMessage]
    public let orchestrator: OrchestratorRuntime?
    public let machine: Machine
    public let usage: Usage?
    public let prs: [PullRequest]?
    public let user: String?
    /// The newest reports, read or not; a task's documents are looked up here.
    public let reports: [Report]?
    public let requests: [WorkRequest]?
    public let tasks: [WorkTask]?
}

public enum InboxItem: Identifiable, Hashable, Sendable {
    case question(Question)
    case report(Report)
    case pullRequest(PullRequest)

    public var id: String {
        switch self {
        case .question(let q): "q\(q.id)"
        case .report(let r): "r\(r.id)"
        case .pullRequest(let p): "p\(p.id)"
        }
    }

    public var isPullRequest: Bool {
        if case .pullRequest = self { true } else { false }
    }

    public var date: Int64 {
        switch self {
        case .question(let q): q.askedAt
        case .report(let r): r.createdAt
        case .pullRequest(let p): p.openedAt
        }
    }

    public var sessionId: String {
        switch self {
        case .question(let q): q.sessionId
        case .report(let r): r.sessionId
        case .pullRequest(let p): p.sessionId
        }
    }
}

public struct SearchResult: Codable, Sendable {
    public let via: String
    public let sessions: [String]
}

extension Attachment {
    public var isText: Bool {
        guard storedPath != nil else { return false }
        let ext = (name as NSString).pathExtension.lowercased()
        return ["md", "markdown", "txt", "text", ""].contains(ext)
    }
}

extension Question {
    public var isPlan: Bool { origin == "plan" }
}

extension InboxItem {
    public var title: String {
        switch self {
        case .question(let q): q.title
        case .report(let r): r.title
        case .pullRequest(let p): p.title
        }
    }

    public var nodeName: String? {
        switch self {
        case .question(let q): q.nodeName
        case .report(let r): r.nodeName
        case .pullRequest(let p): p.nodeName
        }
    }
}
