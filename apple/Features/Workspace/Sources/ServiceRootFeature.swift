import AlondraAttachmentsFeature
import AlondraCommands
import AlondraNewWorkFeature
import AlondraPullRequestsFeature
import AlondraShared
import ComposableArchitecture

@Reducer
public struct ServiceRootFeature {
    public init() {}
    @ObservableState
    public struct State {
        public init() {}
        public enum Tab: Hashable { case inbox, tasks, fleet, newRequest, account }
        public var tab = Tab.inbox
        public var settingsPresented = false
        public var triage = ServiceTriageFeature.State()
        public var tasks = ServiceTasksFeature.State()
        public var fleet = ServiceFleetFeature.State()
        public var newRequest = NewRequestFeature.State()
        @Presents public var pullRequest: ServicePullRequestFeature.State?
        public var pendingInboxID: String?
        public var feedbackResolutions: [ReportFeedbackRecoveryEvent] = []
    }
    public enum Action {
        case appeared, disappeared
        case commandResolved(CommandResolution)
        case feedbackRecovered([ReportFeedbackRecoveryEvent])
        case tabSelected(State.Tab)
        case settingsPresented(Bool)
        case focusInbox(String)
        case triage(ServiceTriageFeature.Action)
        case tasks(ServiceTasksFeature.Action)
        case fleet(ServiceFleetFeature.Action)
        case newRequest(NewRequestFeature.Action)
        case pullRequest(PresentationAction<ServicePullRequestFeature.Action>)
    }
    @Dependency(\.serviceCommandRecovery) var recovery
    private enum RecoveryCancelID { case observation }
    public var body: some ReducerOf<Self> {
        Scope(state: \.triage, action: \.triage) { ServiceTriageFeature() }
        Scope(state: \.tasks, action: \.tasks) { ServiceTasksFeature() }
        Scope(state: \.fleet, action: \.fleet) { ServiceFleetFeature() }
        Scope(state: \.newRequest, action: \.newRequest) { NewRequestFeature() }
        Reduce { state, action in
            switch action {
            case .appeared:
                return .run { send in
                    await withTaskGroup(of: Void.self) { group in
                        group.addTask {
                            for await resolution in await recovery.observe() { await send(.commandResolved(resolution)) }
                        }
                        group.addTask {
                            for await events in await recovery.observeFeedback() { await send(.feedbackRecovered(events)) }
                        }
                    }
                }.cancellable(id: RecoveryCancelID.observation, cancelInFlight: true)
            case .disappeared: return .cancel(id: RecoveryCancelID.observation)
            case .commandResolved(let resolution): return resolutionEffects(resolution, state: state)
            case .feedbackRecovered(let events):
                state.feedbackResolutions = events
                return feedbackResolutionEffects(events, state: state)
            case .triage(.document(.presented(.reportFeedbackFinished))):
                return .merge(feedbackResolutionEffects(state.feedbackResolutions, state: state),
                    .send(.fleet(.hiddenInboxChanged(state.triage.triage.hiddenIDs))), .send(.tasks(.hiddenInboxChanged(state.triage.triage.hiddenIDs))))
            case .tabSelected(let tab): state.tab = tab; return .none
            case .settingsPresented(let value): state.settingsPresented = value; return .none
            case .focusInbox(let id):
                state.tab = .inbox
                state.pendingInboxID = id
                return routePending(&state)
            case .triage(.triage(.inboxUpdated(let update))):
                let routing: Effect<Action> = .merge(routePending(&state),
                    update.identity == state.triage.triage.identity ? .send(.newRequest(.workspaceUpdated(update.overview))) : .none,
                    .send(.fleet(.hiddenInboxChanged(state.triage.triage.hiddenIDs))), .send(.tasks(.hiddenInboxChanged(state.triage.triage.hiddenIDs))))
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
                state.pullRequest = ServicePullRequestFeature.State(pr: pr, reports: state.triage.triage.reports(forPullRequest: pr.id), questions: state.triage.triage.questions(forPullRequest: pr.id))
                return .none
            case .pullRequest(.presented(.pr(.mutationFinished))), .pullRequest(.presented(.diff(.reviewFinished))):
                if let detail = state.pullRequest, detail.pr.shouldDismiss || detail.diff.shouldDismiss {
                    // Closing a successful review does not imply the PR left the inbox.
                    // In particular, asking a question keeps its card until a new snapshot.
                    state.pullRequest = nil
                }
                return .none
            case .newRequest(.appeared): return .send(.newRequest(.availabilityChanged(state.newRequest.orchestratorName != nil)))
            case .triage:
                return .merge(.send(.fleet(.hiddenInboxChanged(state.triage.triage.hiddenIDs))), .send(.tasks(.hiddenInboxChanged(state.triage.triage.hiddenIDs))))
            case .tasks, .pullRequest, .fleet, .newRequest: return .none
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
public struct ServicePullRequestFeature {
    public init() {}
    @ObservableState
    public struct State: Equatable {
        public var downloads = AttachmentDownloadFeature.State()
        public var pr: PullRequestFeature.State
        public var diff: DiffReviewFeature.State
        public var reports: [Report] = []
        public var questions: [Question] = []
        public init(pr: PullRequest, reports: [Report] = [], questions: [Question] = []) {
            self.reports = reports
            self.questions = questions
            self.pr = PullRequestFeature.State(pr: pr)
            diff = DiffReviewFeature.State(prID: pr.id)
        }
    }
    public enum Action {
        case downloads(AttachmentDownloadFeature.Action)
        case pr(PullRequestFeature.Action)
        case diff(DiffReviewFeature.Action)
        case updated(PullRequest?)
        case snapshotUnavailable
    }
    public var body: some ReducerOf<Self> {
        Scope(state: \.downloads, action: \.downloads) { AttachmentDownloadFeature() }
        Scope(state: \.pr, action: \.pr) { PullRequestFeature() }
        Scope(state: \.diff, action: \.diff) { DiffReviewFeature() }
        Reduce { _, action in
            switch action {
            case .updated(let pr): return .merge(.send(.pr(.pullRequestChanged(pr))), .send(.diff(.pullRequestChanged(pr))))
            case .snapshotUnavailable: return .merge(.send(.pr(.snapshotUnavailable)), .send(.diff(.snapshotUnavailable)))
            case .pr, .diff, .downloads: return .none
            }
        }
    }
}
