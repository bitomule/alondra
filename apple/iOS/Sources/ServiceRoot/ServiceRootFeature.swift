import AlondraPullRequestsFeature
import AlondraShared
import ComposableArchitecture

@Reducer
struct ServiceRootFeature {
    @ObservableState
    struct State {
        enum Tab: Hashable { case inbox, tasks, account }
        var tab = Tab.inbox
        var triage = ServiceTriageFeature.State()
        var tasks = ServiceTasksFeature.State()
        @Presents var pullRequest: ServicePullRequestFeature.State?
        var pendingInboxID: String?
    }
    enum Action {
        case tabSelected(State.Tab)
        case focusInbox(String)
        case triage(ServiceTriageFeature.Action)
        case tasks(ServiceTasksFeature.Action)
        case pullRequest(PresentationAction<ServicePullRequestFeature.Action>)
    }
    var body: some ReducerOf<Self> {
        Scope(state: \.triage, action: \.triage) { ServiceTriageFeature() }
        Scope(state: \.tasks, action: \.tasks) { ServiceTasksFeature() }
        Reduce { state, action in
            switch action {
            case .tabSelected(let tab): state.tab = tab; return .none
            case .focusInbox(let id):
                state.tab = .inbox
                state.pendingInboxID = id
                return routePending(&state)
            case .triage(.triage(.inboxUpdated)):
                let routing = routePending(&state)
                guard let detail = state.pullRequest else { return routing }
                guard state.triage.triage.hasSnapshot else {
                    return .merge(routing, .send(.pullRequest(.presented(.snapshotUnavailable))))
                }
                let pr = state.triage.triage.items.compactMap { item -> PullRequest? in
                    if case .pullRequest(let pr) = item, pr.id == detail.pr.id { return pr }
                    return nil
                }.first
                return .merge(routing, .send(.pullRequest(.presented(.updated(pr)))))
            case .triage(.delegate(.pullRequest(let pr))):
                state.pullRequest = ServicePullRequestFeature.State(pr: pr)
                return .none
            case .pullRequest(.presented(.pr(.mutationFinished))), .pullRequest(.presented(.diff(.reviewFinished))):
                if let detail = state.pullRequest, detail.pr.shouldDismiss || detail.diff.shouldDismiss {
                    state.triage.triage.hiddenIDs.insert("p\(detail.pr.id)")
                    state.pullRequest = nil
                }
                return .none
            case .triage, .tasks, .pullRequest: return .none
            }
        }.ifLet(\.$pullRequest, action: \.pullRequest) { ServicePullRequestFeature() }
    }

    private func routePending(_ state: inout State) -> Effect<Action> {
        guard let id = state.pendingInboxID, state.triage.triage.routeTarget(for: id) != nil else { return .none }
        state.pendingInboxID = nil
        return .merge(.send(.triage(.triage(.focus(id)))), .send(.triage(.open(id))))
    }
}

@Reducer
struct ServicePullRequestFeature {
    @ObservableState
    struct State: Equatable {
        var pr: PullRequestFeature.State
        var diff: DiffReviewFeature.State
        init(pr: PullRequest) {
            self.pr = PullRequestFeature.State(pr: pr)
            diff = DiffReviewFeature.State(prID: pr.id)
        }
    }
    enum Action {
        case pr(PullRequestFeature.Action)
        case diff(DiffReviewFeature.Action)
        case updated(PullRequest?)
        case snapshotUnavailable
    }
    var body: some ReducerOf<Self> {
        Scope(state: \.pr, action: \.pr) { PullRequestFeature() }
        Scope(state: \.diff, action: \.diff) { DiffReviewFeature() }
        Reduce { _, action in
            switch action {
            case .updated(let pr): return .merge(.send(.pr(.pullRequestChanged(pr))), .send(.diff(.pullRequestChanged(pr))))
            case .snapshotUnavailable: return .merge(.send(.pr(.snapshotUnavailable)), .send(.diff(.snapshotUnavailable)))
            case .pr, .diff: return .none
            }
        }
    }
}
