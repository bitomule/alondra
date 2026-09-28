import AlondraContracts
import AlondraDomain
import Foundation

extension DomainV2.TaskState {
    /// Every server state keeps its own label; delivered, accepted and finished never merge.
    public var label: String {
        switch self {
        case .notStarted: String(localized: "not started", table: "AlondraShared", bundle: #bundle)
        case .running: String(localized: "running", table: "AlondraShared", bundle: #bundle)
        case .needsYou: String(localized: "needs you", table: "AlondraShared", bundle: #bundle)
        case .inReview: String(localized: "presented", table: "AlondraShared", bundle: #bundle)
        case .waitingDependency: String(localized: "waiting for a dependency", table: "AlondraShared", bundle: #bundle)
        case .closing: String(localized: "accepted, pending the agent", table: "AlondraShared", bundle: #bundle)
        case .done: String(localized: "task-state.done", defaultValue: "finished", table: "AlondraShared", bundle: #bundle)
        case .cancelled: String(localized: "cancelled", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension DomainV2.Request {
    /// The first line of what the user asked, shortened.
    public var headline: String? {
        let line = body.split(whereSeparator: \.isNewline).first.map { String($0).trimmingCharacters(in: .whitespaces) } ?? ""
        guard !line.isEmpty else { return nil }
        return line.count <= 90 ? line : String(line.prefix(89)).trimmingCharacters(in: .whitespaces) + "…"
    }

    public var stateLabel: String {
        closed ? String(localized: "request-state.done", defaultValue: "finished", table: "AlondraShared", bundle: #bundle)
            : String(localized: "in progress", table: "AlondraShared", bundle: #bundle)
    }
}

extension DomainV2.Task {
    public func fleetRowTitle(nodeDisplayName: String) -> String {
        title == nodeDisplayName ? Self.mainTaskLabel : title
    }
    public static var mainTaskLabel: String { String(localized: "main task", table: "AlondraShared", bundle: #bundle) }
}

extension NodeActivity {
    public var thread: String {
        if isMainThread { return String(localized: "main", table: "AlondraShared", bundle: #bundle) }
        return agentType ?? String(localized: "subagent", table: "AlondraShared", bundle: #bundle)
    }
}

extension NodeOccupancy {
    public var label: String {
        switch self {
        case .busy: String(localized: "busy", table: "AlondraShared", bundle: #bundle)
        case .free: String(localized: "free", table: "AlondraShared", bundle: #bundle)
        case .waitingMachine: String(localized: "waiting for the machine", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension Node {
    public var fleetLabel: String { orchestratorRuntime?.label ?? orchestratorState?.label ?? occupancy.label }
}

extension TaskStateGroup {
    public var title: String {
        switch kind {
        case .task(let state):
            switch state {
            case .notStarted: String(localized: "Not started", table: "AlondraShared", bundle: #bundle)
            case .needsYou: String(localized: "Needs you", table: "AlondraShared", bundle: #bundle)
            case .inReview: String(localized: "Presented", table: "AlondraShared", bundle: #bundle)
            case .closing: String(localized: "Accepted, pending the agent", table: "AlondraShared", bundle: #bundle)
            case .waitingDependency: String(localized: "Waiting for a dependency", table: "AlondraShared", bundle: #bundle)
            case .done: String(localized: "task-group.done", defaultValue: "Finished", table: "AlondraShared", bundle: #bundle)
            case .cancelled: String(localized: "Cancelled", table: "AlondraShared", bundle: #bundle)
            case .running: String(localized: "Running", table: "AlondraShared", bundle: #bundle)
            }
        case .archived: String(localized: "Archived", table: "AlondraShared", bundle: #bundle)
        }
    }
}
