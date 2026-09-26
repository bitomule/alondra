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
        public var macPresentation = MacPresentationFeature.State()
        public var tab = Tab.inbox
        public var settingsPresented = false
        public var delivery = DeliveryNoticeFeature.State()
        public var macError: WorkspaceNotice?
        var lastMacErrorMessage: String?
        var macErrorMessage: String? {
            if let error = delivery.acknowledgementError ?? delivery.status?.persistenceError { return error }
            if let error = triage.triage.notice { return error }
            if case .failure(let error) = notice?.event { return error }
            return nil
        }
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
        public func inboxCount(pullRequests: Bool) -> Int {
            triage.triage.queue.count { $0.isPullRequest == pullRequests }
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
        case macPresentation(MacPresentationFeature.Action)
        case delivery(DeliveryNoticeFeature.Action)
        case dismissMacError(UUID)
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
    private enum NoticeCancelID { case timeout, macTimeout }
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
            if case .dismissMacError(let id) = action {
                guard state.macError?.id == id else { return .none }
                state.macError = nil
                return .cancel(id: NoticeCancelID.macTimeout)
            }
            if case .dismissCloseRefusal = action { state.closeRefusal = nil; return .none }
            guard let event = noticeEvent(state: state, action: action) else { return .none }
            if case .closeRefused(let message) = event { state.closeRefusal = message; return .none }
            let notice = WorkspaceNotice(event: event)
            // A repeated failed user action is a new error even when its message is unchanged.
            if notice.failure { state.lastMacErrorMessage = nil }
            state.notice = notice
            return .run { send in
                try await noticeClock.sleep(for: .seconds(notice.failure ? 4 : 2.2))
                await send(.dismissNotice(notice.id))
            }.cancellable(id: NoticeCancelID.timeout, cancelInFlight: true)
        }
        Scope(state: \.macPresentation, action: \.macPresentation) { MacPresentationFeature() }
            .onChange(of: \.macPresentation.selection) { _, _ in
                Reduce { state, _ in
                    state.triage.cancelPendingSubmissions()
                    state.pullRequest?.cancelPendingSubmission()
                    return .none
                }
            }
        Scope(state: \.delivery, action: \.delivery) { DeliveryNoticeFeature() }
        Scope(state: \.triage, action: \.triage) { ServiceTriageFeature() }
        Scope(state: \.tasks, action: \.tasks) { ServiceTasksFeature() }
        Scope(state: \.fleet, action: \.fleet) { ServiceFleetFeature() }
        Scope(state: \.newRequest, action: \.newRequest) { NewRequestFeature() }
        Reduce { state, action in
            switch action {
            case .appeared:
                return .merge(.send(.delivery(.appeared)), .run { send in
                    await withTaskGroup(of: Void.self) { group in
                        group.addTask {
                            for await resolution in await recovery.observe() { await send(.commandResolved(resolution)) }
                        }
                        group.addTask {
                            for await events in await recovery.observeFeedback() { await send(.feedbackRecovered(events)) }
                        }
                    }
                }.cancellable(id: RecoveryCancelID.observation, cancelInFlight: true))
            case .disappeared:
                state.triage.cancelPendingSubmissions()
                state.pullRequest?.cancelPendingSubmission()
                return .merge(.cancel(id: RecoveryCancelID.observation), .send(.delivery(.disappeared)))
            case .commandResolved(let resolution): return resolutionEffects(resolution, state: state)
            case .feedbackRecovered(let events):
                state.feedbackResolutions = events
                return feedbackResolutionEffects(events, state: state)
            case .triage(.document(.presented(.reportFeedbackFinished))),
                 .triage(.cards(.element(_, .document(.reportFeedbackFinished)))):
                return .merge(feedbackResolutionEffects(state.feedbackResolutions, state: state),
                    .send(.fleet(.hiddenInboxChanged(state.triage.triage.hiddenIDs))), .send(.tasks(.hiddenInboxChanged(state.triage.triage.hiddenIDs))))
            case .tabSelected(let tab):
                if state.tab != tab {
                    state.triage.cancelPendingSubmissions()
                    state.pullRequest?.cancelPendingSubmission()
                }
                state.tab = tab
                return .none
            case .settingsPresented(let value): state.settingsPresented = value; return .none
            case .focusInbox(let id):
                state.triage.cancelPendingSubmissions()
                state.pullRequest?.cancelPendingSubmission()
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
            case .macPresentation, .dismissMacError, .delivery, .tasks, .pullRequest, .fleet, .newRequest, .dismissNotice, .dismissCloseRefusal: return .none
            }
        }.ifLet(\.$pullRequest, action: \.pullRequest) { ServicePullRequestFeature() }
        Reduce { state, _ in
            let message = state.macErrorMessage
            guard state.lastMacErrorMessage != message else { return .none }
            state.lastMacErrorMessage = message
            guard let message else { return .none }
            let notice = WorkspaceNotice(event: .failure(message))
            state.macError = notice
            return .run { send in
                try await noticeClock.sleep(for: .seconds(6))
                await send(.dismissMacError(notice.id))
            }.cancellable(id: NoticeCancelID.macTimeout, cancelInFlight: true)
        }
    }

    private func noticeEvent(state: State, action: Action) -> WorkspaceNotice.Event? {
        switch action {
        case .triage(.cards(.element(let cardID, .document(let action)))):
            return state.triage.cards[id: cardID]?.document.flatMap { documentNotice($0, action: action) }
        case .triage(.cards(.element(let cardID, .pullRequest(.diff(.reviewFinished(let outcome, let epoch)))))):
            guard let diff = state.triage.cards[id: cardID]?.pullRequest?.diff,
                  diff.isSending, diff.presentationEpoch == epoch else { return nil }
            return rejectedPRNotice(outcome)
        case .triage(.cards(.element(let cardID, .pullRequest(.pr(.mutationFinished(let outcome, let epoch)))))):
            guard let pr = state.triage.cards[id: cardID]?.pullRequest?.pr,
                  pr.isSending, pr.presentationEpoch == epoch else { return nil }
            return rejectedPRNotice(outcome)
        case .triage(.cards(.element(let cardID, .question(let action)))):
            return state.triage.cards[id: cardID]?.question.flatMap { questionNotice($0, action: action) }
        case .pullRequest(.presented(.attachedQuestion(.element(let id, .question(let action))))):
            return state.pullRequest?.questionFeatures[id: id].flatMap { questionNotice($0.question, action: action) }
        case .triage(.cards(.element(let cardID, .pullRequest(.attachedQuestion(.element(let id, .question(let action))))))):
            return state.triage.cards[id: cardID]?.pullRequest?.questionFeatures[id: id].flatMap { questionNotice($0.question, action: action) }
        case .delivery(.acknowledgementFinished(let scope, let error)):
            guard state.delivery.status?.scope == scope, let error else { return nil }
            return .failure(error)
        case .delivery(.updated(let status)):
            guard let error = status.persistenceError, error != state.delivery.status?.persistenceError else { return nil }
            return .failure(error)
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

    private func documentNotice(_ document: DocumentReviewFeature.State, action: DocumentReviewFeature.Action) -> WorkspaceNotice.Event? {
        switch action {
        case .submissionFinished(let revision, _, let outcome), .reportFeedbackFinished(let revision, let outcome):
            guard document.isSubmitting, document.revision == revision else { return nil }
            switch outcome {
            case .failed(let message), .forbidden(let message), .conflict(let message):
                return .failure(couldNotSendMessage(message))
            default: return nil
            }
        case .commandResolved(let id, let rejection):
            guard document.needsReconciliation, document.uncertainCommandID == id else { return nil }
            return rejection.map { .failure(couldNotSendMessage($0)) }
        default: return nil
        }
    }

    private func rejectedPRNotice(_ outcome: PRMutationOutcome) -> WorkspaceNotice.Event? {
        guard case .rejected(let message) = outcome else { return nil }
        return .failure(couldNotSendMessage(message))
    }

    private func questionNotice(_ question: QuestionFeature.State, action: QuestionFeature.Action) -> WorkspaceNotice.Event? {
        switch action {
        case .submissionFinished(let revision, let outcome):
            guard question.isSubmitting, question.revision == revision else { return nil }
            switch outcome {
            case .failed(let message), .forbidden(let message), .conflict(let message): return .failure(couldNotSendMessage(message))
            default: return nil
            }
        case .commandResolved(let id, let rejection):
            guard question.needsReconciliation, question.uncertainCommandID == id else { return nil }
            return rejection.map { .failure(couldNotSendMessage($0)) }
        case .attachedSubmissionFailed(let message): return .failure(couldNotSendMessage(message))
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
        state.triage.cancelPendingSubmissions()
        state.pullRequest?.cancelPendingSubmission()
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
        public mutating func cancelPendingSubmission() {
            diff.cancelPendingSubmission()
            for id in questionFeatures.ids {
                questionFeatures[id: id]?.question.cancelPendingSubmission()
            }
        }
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
            case .diff(.disappeared):
                state.cancelPendingSubmission()
                return .none
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
