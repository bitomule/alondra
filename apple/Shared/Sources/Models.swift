import Foundation

public enum NodeState: String, Codable, Sendable {
    case working
    case waitingAnswer = "waiting_answer"
    case delivered
    case waitingMachine = "waiting_machine"
    case done
    case unclear

    public var label: String {
        switch self {
        case .working: String(localized: "working", table: "AlondraShared", bundle: #bundle)
        case .waitingAnswer: String(localized: "waiting for an answer", table: "AlondraShared", bundle: #bundle)
        case .delivered: String(localized: "delivering results", table: "AlondraShared", bundle: #bundle)
        case .waitingMachine: String(localized: "waiting for the machine", table: "AlondraShared", bundle: #bundle)
        case .done: String(localized: "done", table: "AlondraShared", bundle: #bundle)
        case .unclear: String(localized: "unclear what it wants", table: "AlondraShared", bundle: #bundle)
        }
    }
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
    public let alive: Bool
    public let isOrchestrator: Bool
    public let hasPlugin: Bool
    public let state: NodeState
    public let stateSince: Int64
    public let stateSource: String
    public let tool: ToolInFlight?
    public let lastMessage: String?
    public let lastMessageAt: Int64?
    public let daemonDetail: String?
    public let daemonNeeds: String?
    public let project: String?

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
    public let nodeContext: String?
    public let status: String
    public let askedAt: Int64

    public var kindLabel: String? {
        switch origin {
        case "permission": String(localized: "Permission", table: "AlondraShared", bundle: #bundle)
        case "plan": String(localized: "Plan", table: "AlondraShared", bundle: #bundle)
        case "ask_user_question": String(localized: "Menu", table: "AlondraShared", bundle: #bundle)
        default: nil
        }
    }
}

public struct Machine: Codable, Hashable, Sendable {
    public let memoryTotal: UInt64
    public let memoryAvailable: UInt64
    public let memoryFreePercent: UInt32?
    public let load: [Double]
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

    public var kindLabel: String {
        switch kind {
        case "notice": String(localized: "Notice", table: "AlondraShared", bundle: #bundle)
        case "document": String(localized: "Document", table: "AlondraShared", bundle: #bundle)
        case "final": String(localized: "Finished", table: "AlondraShared", bundle: #bundle)
        default: String(localized: "Progress", table: "AlondraShared", bundle: #bundle)
        }
    }
}

public struct SentMessage: Codable, Identifiable, Hashable, Sendable {
    public let id: UInt64
    public let from: String
    public let text: String
    public let createdAt: Int64
    public let handedAt: Int64?
    public let deliveredAt: Int64?
}

public struct Overview: Codable, Sendable {
    public let tick: UInt64
    public let nodes: [Node]
    public let openQuestions: [Question]
    public let news: [Report]
    public let orchestratorMessages: [SentMessage]
    public let machine: Machine
}

public func ago(_ millis: Int64) -> String {
    let seconds = max(0, Int64(Date().timeIntervalSince1970 * 1000) - millis) / 1000
    switch seconds {
    case ..<60: return String(localized: "\(seconds) s", table: "AlondraShared", bundle: #bundle)
    case ..<3600: return String(localized: "\(seconds / 60) min", table: "AlondraShared", bundle: #bundle)
    case ..<86400: return String(localized: "\(seconds / 3600) h", table: "AlondraShared", bundle: #bundle)
    default: return String(localized: "\(seconds / 86400) d", table: "AlondraShared", bundle: #bundle)
    }
}

public enum InboxItem: Identifiable, Hashable {
    case question(Question)
    case report(Report)

    public var id: String {
        switch self {
        case .question(let q): "q\(q.id)"
        case .report(let r): "r\(r.id)"
        }
    }

    public var date: Int64 {
        switch self {
        case .question(let q): q.askedAt
        case .report(let r): r.createdAt
        }
    }

    public var sessionId: String {
        switch self {
        case .question(let q): q.sessionId
        case .report(let r): r.sessionId
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
        }
    }

    public var nodeName: String? {
        switch self {
        case .question(let q): q.nodeName
        case .report(let r): r.nodeName
        }
    }
}
