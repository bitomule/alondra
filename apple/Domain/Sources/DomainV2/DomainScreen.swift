import AlondraContracts

/// A screen that reads the synced workspace. The app fetches only what the inbox shows when it
/// opens; a screen names itself here when it appears and the client loads what it needs.
public enum DomainScreen: Equatable, Sendable, CaseIterable {
    case inbox, tasks, fleet, newRequest, pullRequest, machine

    public var sections: Set<DomainV2.Section> {
        switch self {
        case .inbox:
            [.tasks, .agents, .requests, .pullRequests, .documents, .interactions]
        case .tasks:
            [.tasks, .agents, .requests, .pullRequests, .documents, .interactions,
             .stackReadiness, .mergeAuthorizations, .supersededAcceptances]
        case .fleet:
            [.agents, .tasks, .requests]
        case .newRequest:
            [.requests, .tasks]
        case .pullRequest:
            [.pullRequests, .mergeAuthorizations, .stackReadiness, .tasks, .documents]
        case .machine:
            []
        }
    }

    public var needsRuntime: Bool {
        switch self {
        case .inbox: false
        case .tasks, .fleet, .newRequest, .pullRequest, .machine: true
        }
    }
}
