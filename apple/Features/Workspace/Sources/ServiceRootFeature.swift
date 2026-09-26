import AlondraAttachmentsFeature
import AlondraFleetFeature
import Foundation
import AlondraCommands
import AlondraNewWorkFeature
import AlondraQuestionsFeature
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
        public var notice: WorkspaceNotice?
        public var closeRefusal: String?
        public var triage = ServiceTriageFeature.State()
        public var tasks = ServiceTasksFeature.State()
        public var fleet = ServiceFleetFeature.State()
        public var newRequest = NewRequestFeature.State()
        @Presents public var pullRequest: ServicePullRequestFeature.State?
        public var pendingInboxID: String?
        public var feedbackResolutions: [ReportFeedbackRecoveryEvent] = []
        public var macSubtitle: String {
            guard triage.triage.hasSnapshot, let overview = fleet.overview else {
                return String(localized: "No connection to alondrad")
            }
            let count = triage.triage.queue.count
            let pending = count == 0 ? String(localized: "All caught up") : String(localized: "\(count) pending")
            return [pending, String(localized: "\(overview.nodes.count) nodes")].joined(separator: " · ")
        }
        public func asksOnPullRequest(_ item: InboxItem) -> Bool {
            guard case .pullRequest(let pr) = item else { return false }
            return !triage.triage.questions(forPullRequest: pr.id).isEmpty
        }
        public func taskTitle(forInboxID id: String) -> String? {
            guard let item = triage.triage.items.first(where: { $0.id == id }) else { return nil }
            return tasks.taskBoard.task(for: item)?.title
        }
    }
    public enum Action {
        case appeared, disappeared
        case dismissNotice(UUID)
        case dismissCloseRefusal
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
    @Dependency(\.continuousClock) var noticeClock
    private enum NoticeCancelID { case timeout }
    @Dependency(\.serviceCommandRecovery) var recovery
    private enum RecoveryCancelID { case observation }
    public var body: some ReducerOf<Self> {
        // Observe the accepted operation before child navigation removes its state.
        Reduce { state, action in
            if case .dismissNotice(let id) = action {
                guard state.notice?.id == id else { return .none }
                state.notice = nil
                return .cancel(id: NoticeCancelID.timeout)
            }
            if case .dismissCloseRefusal = action { state.closeRefusal = nil; return .none }
            guard let event = noticeEvent(state: state, action: action) else { return .none }
            if case .closeRefused(let message) = event { state.closeRefusal = message; return .none }
            let notice = WorkspaceNotice(event: event)
            state.notice = notice
            return .run { send in
                try await noticeClock.sleep(for: .seconds(notice.failure ? 4 : 2.2))
                await send(.dismissNotice(notice.id))
            }.cancellable(id: NoticeCancelID.timeout, cancelInFlight: true)
        }
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
                return .merge(routing, .send(.pullRequest(.presented(.updated(pr)))), .send(.pullRequest(.presented(.contextUpdated(reports: state.triage.triage.reports(forPullRequest: detail.pr.id), questions: state.triage.triage.questions(forPullRequest: detail.pr.id))))))
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
            case .tasks, .pullRequest, .fleet, .newRequest, .dismissNotice, .dismissCloseRefusal: return .none
            }
        }.ifLet(\.$pullRequest, action: \.pullRequest) { ServicePullRequestFeature() }
    }

    private func noticeEvent(state: State, action: Action) -> WorkspaceNotice.Event? {
        switch action {
        case .newRequest(.submitFinished(let result, let epoch)):
            guard state.newRequest.isSubmitting, epoch == state.newRequest.presentationEpoch else { return nil }
            switch result {
            case .confirmed: return .sentToOrchestrator
            case .failed(let message): return .failure(message)
            default: return nil
            }
        case .newRequest(.commandResolved(let id, let rejection)):
            guard state.newRequest.submissionUncertain, state.newRequest.uncertainCommandID == id else { return nil }
            return rejection.map(WorkspaceNotice.Event.failure) ?? .sentToOrchestrator
        case .fleet(.detail(.presented(let action))):
            return state.fleet.detail.flatMap { nodeNotice($0, action: action) }
        case .fleet(.path(.element(let id, .node(let action)))):
            guard case .node(let node) = state.fleet.path[id: id] else { return nil }
            return nodeNotice(node, action: action)
        default: return nil
        }
    }

    private func nodeNotice(_ node: NodeDetailFeature.State, action: NodeDetailFeature.Action) -> WorkspaceNotice.Event? {
        switch action {
        case .sendFinished(_, let outcome, let epoch):
            guard node.isSending, epoch == node.presentationEpoch else { return nil }
            switch outcome {
            case .confirmed: return .messageSent(node.node?.displayName ?? node.sessionID)
            case .rejected(let message): return .failure(message)
            default: return nil
            }
        case .closeFinished(let outcome, let epoch):
            guard node.isClosing, epoch == node.presentationEpoch else { return nil }
            switch outcome {
            case .confirmed: return .closed(node.node?.displayName ?? node.sessionID)
            case .rejected(let message): return .closeRefused(message)
            default: return nil
            }
        default: return nil
        }
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
        public var questionFeatures: IdentifiedArrayOf<AttachedPRQuestionFeature.State> = []
        public var descriptionExpanded: Bool?
        public init(pr: PullRequest, reports: [Report] = [], questions: [Question] = []) {
            self.reports = reports
            self.questions = questions
            questionFeatures = IdentifiedArray(uniqueElements: questions.map(AttachedPRQuestionFeature.State.init))
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
        case contextUpdated(reports: [Report], questions: [Question])
        case toggleDescription
        case attachedQuestion(IdentifiedActionOf<AttachedPRQuestionFeature>)
    }
    public var body: some ReducerOf<Self> {
        Scope(state: \.downloads, action: \.downloads) { AttachmentDownloadFeature() }
        Scope(state: \.pr, action: \.pr) { PullRequestFeature() }
        Scope(state: \.diff, action: \.diff) { DiffReviewFeature() }
        Reduce { state, action in
            switch action {
            case .contextUpdated(let reports, let questions):
                state.reports = reports
                state.questions = questions
                let ids = Set(questions.map(\.id))
                state.questionFeatures.removeAll { !ids.contains($0.id) }
                var effects: [Effect<Action>] = []
                for question in questions {
                    if state.questionFeatures[id: question.id] == nil {
                        state.questionFeatures.append(AttachedPRQuestionFeature.State(question))
                    } else {
                        effects.append(.send(.attachedQuestion(.element(id: question.id, action: .question(.refreshed(question))))))
                    }
                }
                return .merge(effects)
            case .toggleDescription:
                state.descriptionExpanded = !(state.descriptionExpanded ?? ((state.pr.pr?.body?.count ?? 0) <= 600))
                return .none
            case .updated(let pr): return .merge(.send(.pr(.pullRequestChanged(pr))), .send(.diff(.pullRequestChanged(pr))))
            case .snapshotUnavailable: return .merge(.send(.pr(.snapshotUnavailable)), .send(.diff(.snapshotUnavailable)))
            case .pr, .diff, .downloads, .attachedQuestion: return .none
            }
        }
        .forEach(\.questionFeatures, action: \.attachedQuestion) { AttachedPRQuestionFeature() }
    }
}

@Reducer
public struct AttachedPRQuestionFeature {
    @ObservableState
    public struct State: Equatable, Identifiable {
        public var id: UInt64 { question.question.id }
        public var question: QuestionFeature.State
        public init(_ value: Question) { question = QuestionFeature.State(question: value) }
    }
    public enum Action { case question(QuestionFeature.Action) }
    public init() {}
    public var body: some ReducerOf<Self> {
        Scope(state: \.question, action: \.question) { QuestionFeature() }
    }
}

public struct WorkspaceNotice: Equatable, Sendable, Identifiable {
    public enum Event: Equatable, Sendable {
        case sentToOrchestrator, messageSent(String), closed(String), failure(String), closeRefused(String)
    }
    public let id = UUID()
    public let event: Event
    public var failure: Bool {
        if case .failure = event { return true }
        return false
    }
    public init(event: Event) { self.event = event }
}
