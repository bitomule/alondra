import Foundation
import AlondraContracts
import AlondraDomain

extension NodeState {
    public var label: String {
        switch self {
        case .working: String(localized: "working", table: "AlondraShared", bundle: #bundle)
        case .waitingAnswer: String(localized: "waiting for an answer", table: "AlondraShared", bundle: #bundle)
        case .yourTurn: String(localized: "your turn", table: "AlondraShared", bundle: #bundle)
        case .waitingMerge: String(localized: "waiting for your review", table: "AlondraShared", bundle: #bundle)
        case .delivered: String(localized: "delivering results", table: "AlondraShared", bundle: #bundle)
        case .waitingMachine: String(localized: "waiting for the machine", table: "AlondraShared", bundle: #bundle)
        case .done: String(localized: "done", table: "AlondraShared", bundle: #bundle)
        case .unclear: String(localized: "unclear what it wants", table: "AlondraShared", bundle: #bundle)
        case .stalled: String(localized: "stalled", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension OrchestratorState {
    public var label: String {
        switch self {
        case .onDuty: String(localized: "on duty", table: "AlondraShared", bundle: #bundle)
        case .handling: String(localized: "handling a request", table: "AlondraShared", bundle: #bundle)
        case .waitingForYou: String(localized: "waiting for you", table: "AlondraShared", bundle: #bundle)
        case .offline: String(localized: "offline", table: "AlondraShared", bundle: #bundle)
        case .unknown: String(localized: "unknown", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension Node {
    public var backgroundBadge: BackgroundBadge? { BackgroundBadge(running) }
    public var stateLabel: String { orchestratorRuntime?.label ?? orchestratorState?.label ?? state.label }
    public static var otherProjectName: String {
        String(localized: "others", table: "AlondraShared", bundle: #bundle)
    }
    public var projectName: String? {
        if isOrchestrator { return String(localized: "Orchestrator", table: "AlondraShared", bundle: #bundle) }
        if project == "others" { return Self.otherProjectName }
        return project
    }
}

extension Question {
    public var kindLabel: String? {
        switch origin {
        case "permission": String(localized: "Permission", table: "AlondraShared", bundle: #bundle)
        case "plan": String(localized: "Plan", table: "AlondraShared", bundle: #bundle)
        case "ask_user_question": String(localized: "Menu", table: "AlondraShared", bundle: #bundle)
        default: nil
        }
    }
}

extension Report {
    public static var seemsToAskLabel: String {
        String(localized: "Seems to ask for something", table: "AlondraShared", bundle: #bundle)
    }
    public var kindLabel: String {
        switch kind {
        case "notice": String(localized: "Notice", table: "AlondraShared", bundle: #bundle)
        case "document": String(localized: "Document", table: "AlondraShared", bundle: #bundle)
        case "final": String(localized: "Finished", table: "AlondraShared", bundle: #bundle)
        default: String(localized: "Progress", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension Usage {
    public var summary: String {
        let week = Int(weekPercent.rounded())
        guard let spent = todaySpent, let share = todayShare else {
            return String(localized: "week \(week)%", table: "AlondraShared", bundle: #bundle)
        }
        return String(localized: "week \(week)% · today \(Int(spent.rounded()))/\(Int(share.rounded()))%", table: "AlondraShared", bundle: #bundle)
    }
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

extension InboxItem {
    /// The opening of the node's mission, on cards Alondra built from its terminal.
    public var missionExcerpt: String? {
        let mission: String? = switch self {
        case .question(let q): q.mission
        case .report(let r): r.mission
        case .pullRequest: nil
        }
        let flat = (mission ?? "").split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard !flat.isEmpty else { return nil }
        return flat.count <= 240 ? flat : String(flat.prefix(240)).trimmingCharacters(in: .whitespaces) + "…"
    }
}


extension OrchestratorRuntime {
    public var label: String {
        if cancelRequested == true { return String(localized: "stopping", table: "AlondraShared", bundle: #bundle) }
        return switch phase {
        case .idle: String(localized: "available", table: "AlondraShared", bundle: #bundle)
        case .starting: String(localized: "starting", table: "AlondraShared", bundle: #bundle)
        case .running: String(localized: "handling a request", table: "AlondraShared", bundle: #bundle)
        case .compacting: String(localized: "compacting context", table: "AlondraShared", bundle: #bundle)
        case .failed where retryAt != nil: String(localized: "retrying", table: "AlondraShared", bundle: #bundle)
        case .failed: String(localized: "needs attention", table: "AlondraShared", bundle: #bundle)
        case .unknown: String(localized: "unknown", table: "AlondraShared", bundle: #bundle)
        }
    }

    public var queueLabel: String {
        String(localized: "Queued messages: \(queueDepth ?? 0)", table: "AlondraShared", bundle: #bundle)
    }
    public var availabilityDescription: String {
        String(localized: "Starts when needed and keeps the conversation between requests.", table: "AlondraShared", bundle: #bundle)
    }
}
