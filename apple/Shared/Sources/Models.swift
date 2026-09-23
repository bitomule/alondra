import Foundation

enum NodeState: String, Codable, Sendable {
    case working
    case waitingAnswer = "waiting_answer"
    case delivered
    case waitingMachine = "waiting_machine"
    case done
    case unclear

    var label: String {
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

struct ToolInFlight: Codable, Hashable, Sendable {
    let name: String
    let summary: String?
    let since: Int64
}

struct Node: Codable, Identifiable, Hashable, Sendable {
    var id: String { sessionId }
    let sessionId: String
    let shortId: String
    let name: String?
    let cwd: String?
    let alive: Bool
    let isOrchestrator: Bool
    let hasPlugin: Bool
    let state: NodeState
    let stateSince: Int64
    let stateSource: String
    let tool: ToolInFlight?
    let lastMessage: String?
    let lastMessageAt: Int64?
    let daemonDetail: String?
    let daemonNeeds: String?

    var displayName: String { name ?? shortId }

    var summary: String {
        if let tool { return "\(tool.name) \(tool.summary ?? "")" }
        return lastMessage ?? daemonNeeds ?? daemonDetail ?? ""
    }
}

struct QuestionOption: Codable, Hashable, Sendable {
    let label: String
    let description: String?
}

struct Link: Codable, Hashable, Sendable {
    let url: String
    let title: String?
}

struct Attachment: Codable, Hashable, Sendable {
    let name: String
    let sourcePath: String
    let storedPath: String?
}

struct Question: Codable, Identifiable, Hashable, Sendable {
    let id: UInt64
    let sessionId: String
    let nodeName: String?
    let title: String
    let detail: String?
    let options: [QuestionOption]
    let links: [Link]
    let attachments: [Attachment]
    let origin: String
    let nodeContext: String?
    let status: String
    let askedAt: Int64

    var kindLabel: String? {
        switch origin {
        case "permission": "Permiso"
        case "plan": "Plan"
        case "ask_user_question": "Menú"
        default: nil
        }
    }
}

struct Machine: Codable, Hashable, Sendable {
    let memoryTotal: UInt64
    let memoryAvailable: UInt64
    let memoryFreePercent: UInt32?
    let load: [Double]
    let diskAvailable: UInt64
    let unreliable: Bool
}

struct Report: Codable, Identifiable, Hashable, Sendable {
    let id: UInt64
    let sessionId: String
    let nodeName: String?
    let title: String
    let body: String?
    let links: [Link]
    let attachments: [Attachment]
    let kind: String
    let createdAt: Int64

    var kindLabel: String {
        switch kind {
        case "delivery": "Entrega"
        case "finding": "Hallazgo"
        case "final": "Terminado"
        default: "Progreso"
        }
    }
}

struct SentMessage: Codable, Identifiable, Hashable, Sendable {
    let id: UInt64
    let from: String
    let text: String
    let createdAt: Int64
    let handedAt: Int64?
    let deliveredAt: Int64?
}

struct Overview: Codable, Sendable {
    let tick: UInt64
    let nodes: [Node]
    let openQuestions: [Question]
    let news: [Report]
    let orchestratorMessages: [SentMessage]
    let machine: Machine
}

func ago(_ millis: Int64) -> String {
    let seconds = max(0, Int64(Date().timeIntervalSince1970 * 1000) - millis) / 1000
    switch seconds {
    case ..<60: return "\(seconds) s"
    case ..<3600: return "\(seconds / 60) min"
    case ..<86400: return "\(seconds / 3600) h"
    default: return "\(seconds / 86400) d"
    }
}
