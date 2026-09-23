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
        case .working: "trabajando"
        case .waitingAnswer: "esperando respuesta"
        case .delivered: "entrega resultado"
        case .waitingMachine: "esperando máquina"
        case .done: "terminado"
        case .unclear: "no sé qué quiere"
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
        case "permission": "Permiso"
        case "plan": "Plan"
        case "ask_user_question": "Menú"
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
        case "notice": "Aviso"
        case "document": "Documento"
        case "final": "Terminado"
        default: "Progreso"
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
    case ..<60: return "\(seconds) s"
    case ..<3600: return "\(seconds / 60) min"
    case ..<86400: return "\(seconds / 3600) h"
    default: return "\(seconds / 86400) d"
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
