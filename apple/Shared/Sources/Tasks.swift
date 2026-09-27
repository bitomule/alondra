import AlondraContracts
import AlondraDomain
import Foundation

extension WorkRequest.State {
    public var label: String {
        switch self {
        case .defining: String(localized: "defining", table: "AlondraShared", bundle: #bundle)
        case .inProgress: String(localized: "in progress", table: "AlondraShared", bundle: #bundle)
        case .done: String(localized: "request-state.done", defaultValue: "finished", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension WorkTask.State {
    public var label: String {
        switch shown {
        case .needsYou: String(localized: "needs you", table: "AlondraShared", bundle: #bundle)
        case .done: String(localized: "task-state.done", defaultValue: "finished", table: "AlondraShared", bundle: #bundle)
        case .cancelled: String(localized: "cancelled", table: "AlondraShared", bundle: #bundle)
        default: String(localized: "running", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension WorkRequest {
    public func definingLabel(now: Date = .now) -> String {
        isLeftDefining(now: now) ? String(localized: "no tasks", table: "AlondraShared", bundle: #bundle) : State.defining.label
    }
}

extension WorkTask {
    public func fleetRowTitle(nodeDisplayName: String) -> String {
        title == nodeDisplayName ? WorkTask.mainTaskLabel : title
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
        case .defining: String(localized: "Defining", table: "AlondraShared", bundle: #bundle)
        case .task(let state):
            switch state.shown {
            case .needsYou: String(localized: "Needs you", table: "AlondraShared", bundle: #bundle)
            case .done: String(localized: "task-group.done", defaultValue: "Finished", table: "AlondraShared", bundle: #bundle)
            case .cancelled: String(localized: "Cancelled", table: "AlondraShared", bundle: #bundle)
            default: String(localized: "Running", table: "AlondraShared", bundle: #bundle)
            }
        case .archived: String(localized: "Archived", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension TaskBoard {
    public func stateLabel(of task: WorkTask) -> String { task.state.label }
}
