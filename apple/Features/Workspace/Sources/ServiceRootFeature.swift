import AlondraAttachmentsFeature
import AlondraFleetFeature
import AlondraDomain
import AlondraTasksFeature
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
            if let error = triage.triage.notice, !triage.triage.noticeIsMutationFailure { return error }
            if case .failure(let error) = notice?.event { return error }
            return nil
        }
        public var notice: WorkspaceNotice?
        public var closeRefusal: String?
        public var triage = ServiceTriageFeature.State()
        public var tasks = ServiceTasksFeature.State()
        public var fleet = ServiceFleetFeature.State()
        public var newRequest = NewRequestFeature.State()
        public var pullRequests: IdentifiedArrayOf<ServicePullRequestFeature.State> = []
        public var pullRequestID: UInt64?
        public var pullRequest: ServicePullRequestFeature.State? {
            get { pullRequestID.flatMap { pullRequests[id: $0] } }
            set {
                guard let value = newValue else { pullRequestID = nil; return }
                pullRequests[id: value.id] = value
                pullRequestID = value.id
            }
        }
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
        case pullRequests(IdentifiedActionOf<ServicePullRequestFeature>)
        case pullRequestDismissed
    }
    @Dependency(\.continuousClock) var noticeClock
    private enum NoticeCancelID { case timeout, macTimeout }
    @Dependency(\.serviceCommandRecovery) var recovery
    private enum RecoveryCancelID { case observation }
    public var body: some ReducerOf<Self> {
        // Observe the accepted operation before child navigation removes its state.
        Reduce { state, action in
            if case .triage(.triage(.identityChanged(let identity))) = action,
               state.triage.triage.identity != identity {
                state.pullRequestID = nil
                state.pullRequests = []
            }

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
                    state.fleet.cancelPendingSubmissions()
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
            // Triage owns the scoped sync connection; snapshots alone cannot express offline.
            state.tasks.connection = state.triage.triage.connection
            switch action {
            case .appeared:
                return .merge(.send(.delivery(.appeared)), .send(.triage(.triage(.appeared))), .run { send in
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
                state.fleet.cancelPendingSubmissions()
                state.pullRequest?.cancelPendingSubmission()
                return .merge(.cancel(id: RecoveryCancelID.observation), .send(.delivery(.disappeared)), .send(.triage(.triage(.disappeared))))
            case .commandResolved(let resolution): return resolutionEffects(resolution, state: state)
            case .feedbackRecovered(let events):
                state.feedbackResolutions = events
                return feedbackResolutionEffects(events, state: state)
            case .triage(.documents(.element(_, .reportFeedbackFinished))),
                 .triage(.cards(.element(_, .document(.reportFeedbackFinished)))):
                return .merge(feedbackResolutionEffects(state.feedbackResolutions, state: state),
                    .send(.fleet(.hiddenInboxChanged(state.triage.triage.hiddenIDs))), .send(.tasks(.hiddenInboxChanged(state.triage.triage.hiddenIDs))))
            case .tabSelected(let tab):
                if state.tab != tab {
                    state.triage.cancelPendingSubmissions()
                    state.fleet.cancelPendingSubmissions()
                    state.pullRequest?.cancelPendingSubmission()
                }
                state.tab = tab
                return .none
            case .settingsPresented(let value): state.settingsPresented = value; return .none
            case .focusInbox(let id):
                state.triage.cancelPendingSubmissions()
                state.fleet.cancelPendingSubmissions()
                state.pullRequest?.cancelPendingSubmission()
                state.tab = .inbox
                state.pendingInboxID = id
                return routePending(&state)
            case .triage(.triage(.identityChanged(let identity))):
                // Run removal through the owning reducer so forEach observes the removed
                // IDs and cancels their scoped effects in this same identity transition.
                // A key-path Scope uses this same navigation cancellation namespace.
                return ServiceFleetFeature().reduce(into: &state.fleet, action: .identityChanged(identity))
                    .map(Action.fleet)
            case .triage(.triage(.inboxUpdated(let update))):
                let routing: Effect<Action> = .merge(routePending(&state),
                    update.identity == state.triage.triage.identity ? .send(.newRequest(.workspaceUpdated(update.overview))) : .none,
                    .send(.fleet(.hiddenInboxChanged(state.triage.triage.hiddenIDs))), .send(.tasks(.hiddenInboxChanged(state.triage.triage.hiddenIDs))))
                guard update.identity == state.triage.triage.identity else { return routing }
                var updates: [Effect<Action>] = [routing]
                for detail in state.pullRequests {
                    let id = detail.id
                    guard state.triage.triage.hasSnapshot else {
                        updates.append(.send(.pullRequests(.element(id: id, action: .snapshotUnavailable))))
                        continue
                    }
                    let pr = state.triage.triage.items.compactMap { item -> PullRequest? in
                        if case .pullRequest(let pr) = item, pr.id == id { return pr }
                        return nil
                    }.first
                    updates.append(.send(.pullRequests(.element(id: id, action: .updated(pr)))))
                    updates.append(.send(.pullRequests(.element(id: id, action: .contextUpdated(
                        reports: state.triage.triage.reports(forPullRequest: id),
                        questions: state.triage.triage.questions(forPullRequest: id))))))
                }
                return .merge(updates)
            case .triage(.delegate(.pullRequest(let pr))):
                state.pullRequest?.cancelPendingSubmission()
                if state.pullRequests[id: pr.id] == nil {
                    state.pullRequest = ServicePullRequestFeature.State(pr: pr, reports: state.triage.triage.reports(forPullRequest: pr.id), questions: state.triage.triage.questions(forPullRequest: pr.id))
                } else {
                    state.pullRequestID = pr.id
                    if state.pullRequests[id: pr.id]?.diff.shouldDismiss == true {
                        state.pullRequests[id: pr.id]?.diff = DiffReviewFeature.State(prID: pr.id)
                    }
                }
                return .send(.pullRequests(.element(id: pr.id, action: .updated(pr))))
            case .pullRequestDismissed:
                state.pullRequest?.cancelPendingSubmission()
                state.pullRequestID = nil
                return .none
            case .pullRequests(.element(let id, .pr(.mutationFinished))), .pullRequests(.element(let id, .diff(.reviewFinished))):
                if let detail = state.pullRequests[id: id], detail.pr.shouldDismiss || detail.diff.shouldDismiss,
                   state.pullRequestID == id {
                    state.pullRequestID = nil
                }
                return .none
            case .newRequest(.appeared): return .send(.newRequest(.availabilityChanged(state.newRequest.orchestratorName != nil)))
            case .triage:
                return .merge(.send(.fleet(.hiddenInboxChanged(state.triage.triage.hiddenIDs))), .send(.tasks(.hiddenInboxChanged(state.triage.triage.hiddenIDs))))
            case .macPresentation, .dismissMacError, .delivery, .tasks, .pullRequests, .fleet, .newRequest, .dismissNotice, .dismissCloseRefusal: return .none
            }
        }.forEach(\.pullRequests, action: \.pullRequests) { ServicePullRequestFeature() }
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
        case .triage(.question(.presented(let action))):
            return state.triage.question.flatMap { questionNotice($0, action: action) }
        case .pullRequests(.element(let id, .diff(.attachmentPicker(let action)))):
            return state.pullRequests[id: id].flatMap { diffUploadNotice($0.diff, action: action) }
        case .triage(.cards(.element(let id, .pullRequest(.diff(.attachmentPicker(let action)))))):
            return state.triage.cards[id: id]?.pullRequest.flatMap { diffUploadNotice($0.diff, action: action) }
        case .triage(.triage(.mutationFinished(let id, let identity, let outcome))):
            guard state.triage.triage.identity == identity, state.triage.triage.pendingIDs.contains(id),
                  case .rejected(let reason) = outcome else { return nil }
            return .failure(couldNotSendMessage(reason))
        case .triage(.triage(.commandResolved(let id, let rejection))):
            guard state.triage.triage.uncertainByItem.values.contains(id) else { return nil }
            return rejection.map { .failure(couldNotSendMessage($0)) }
        case .triage(.documents(.element(let id, let action))):
            return state.triage.documents[id: id].flatMap { documentNotice($0, action: action) }
        case .pullRequests(.element(let id, .diff(.reviewFinished(let outcome, let epoch)))):
            guard let diff = state.pullRequests[id: id]?.diff, diff.isSending, diff.presentationEpoch == epoch else { return nil }
            return rejectedPRNotice(outcome)
        case .pullRequests(.element(let id, .pr(.mutationFinished(let outcome, let epoch)))):
            guard let pr = state.pullRequests[id: id]?.pr, pr.isSending, pr.presentationEpoch == epoch else { return nil }
            return rejectedPRNotice(outcome)
        case .tasks(.board(.archiveFinished(let outcome, let epoch))):
            guard state.tasks.board.isArchiving, state.tasks.board.presentationEpoch == epoch else { return nil }
            return rejectedTaskNotice(outcome)
        case .tasks(.board(.commandResolved(let id, let rejection))):
            guard state.tasks.board.archiveOutcome?.uncertainID == id else { return nil }
            return rejection.map { .failure(couldNotSendMessage($0)) }
        case .tasks(.withdrawFinished(let id, let outcome)):
            guard state.tasks.isWithdrawing, state.tasks.withdrawOperationRequestID == id else { return nil }
            return rejectedTaskNotice(outcome)
        case .tasks(.withdrawalResolved(let id, let rejection)):
            guard state.tasks.withdrawOutcome?.uncertainID == id else { return nil }
            return rejection.map { .failure(couldNotSendMessage($0)) }
        case .tasks(.detail(.presented(let action))):
            return state.tasks.detail.flatMap { taskNotice($0, action: action) }
        case .tasks(.taskPath(.element(let id, let action))):
            return state.tasks.taskPath[id: id].flatMap { taskNotice($0, action: action) }
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
        case .pullRequests(.element(let prID, .attachedQuestion(.element(let id, .question(let action))))):
            return state.pullRequests[id: prID]?.questionFeatures[id: id].flatMap { questionNotice($0.question, action: action) }
        case .triage(.cards(.element(let cardID, .pullRequest(.attachedQuestion(.element(let id, .question(let action))))))):
            return state.triage.cards[id: cardID]?.pullRequest?.questionFeatures[id: id].flatMap { questionNotice($0.question, action: action) }
        case .delivery(.acknowledgementFinished(let scope, let error)):
            guard state.delivery.status?.scope == scope, let error else { return nil }
            return .failure(error)
        case .delivery(.updated(let status)):
            guard let error = status.persistenceError, error != state.delivery.status?.persistenceError else { return nil }
            return .failure(error)
        case .newRequest(.attachmentPicker(.uploaded(let id, .failure(let error)))):
            guard state.newRequest.pendingSubmission != nil,
                  state.newRequest.attachmentPicker.entries.first(where: { $0.id == id })?.status == .uploading else { return nil }
            return .failure(couldNotSendMessage(error.message))
        case .newRequest(.submitFinished(let result, let epoch)):
            guard state.newRequest.isSubmitting, epoch == state.newRequest.presentationEpoch else { return nil }
            switch result {
            case .confirmed: return .sentToOrchestrator
            case .failed(let message): return .failure(couldNotSendMessage(message))
            default: return nil
            }
        case .newRequest(.commandResolved(let id, let rejection)):
            guard state.newRequest.submissionUncertain, state.newRequest.uncertainCommandID == id else { return nil }
            return rejection.map { .failure(couldNotSendMessage($0)) } ?? .sentToOrchestrator
        case .fleet(.fleet(.closeFinished(let id, let outcome, let epoch))):
            let fleet = state.fleet.fleet
            guard fleet.presentationEpoch == epoch, fleet.closingIDs.contains(id) else { return nil }
            switch outcome {
            case .confirmed: return .closed(fleet.snapshot?.node(id)?.node.displayName ?? id)
            case .rejected(let message): return .closeRefused(message)
            default: return nil
            }
        case .fleet(.fleet(.commandResolved(let commandID, let rejection))):
            let fleet = state.fleet.fleet
            guard let id = fleet.closeOutcomeByID.first(where: { $0.value.uncertainID == commandID })?.key else { return nil }
            return rejection.map(WorkspaceNotice.Event.closeRefused) ?? .closed(fleet.snapshot?.node(id)?.node.displayName ?? id)
        case .fleet(.nodes(.element(let id, let action))):
            return state.fleet.nodes[id: id].flatMap { nodeNotice($0, action: action) }
        default: return nil
        }
    }

    private func rejectedTaskNotice(_ outcome: TaskMutationOutcome) -> WorkspaceNotice.Event? {
        guard case .rejected(let message) = outcome else { return nil }
        return .failure(couldNotSendMessage(message))
    }

    private func taskNotice(_ task: TaskDetailFeature.State, action: TaskDetailFeature.Action) -> WorkspaceNotice.Event? {
        switch action {
        case .mutationFinished(let outcome, _, let epoch):
            guard task.isActing, task.presentationEpoch == epoch else { return nil }
            return rejectedTaskNotice(outcome)
        case .commandResolved(let id, let rejection):
            guard task.mutationOutcome?.uncertainID == id else { return nil }
            return rejection.map { .failure(couldNotSendMessage($0)) }
        default: return nil
        }
    }

    private func documentNotice(_ document: DocumentReviewFeature.State, action: DocumentReviewFeature.Action) -> WorkspaceNotice.Event? {
        switch action {
        case .attachmentPicker(.uploaded(let id, .failure(let error))):
            guard document.pendingSubmission != nil,
                  document.attachmentPicker.entries.first(where: { $0.id == id })?.status == .uploading else { return nil }
            return .failure(couldNotSendMessage(error.message))
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

    private func diffUploadNotice(_ diff: DiffReviewFeature.State, action: AttachmentFeature.Action) -> WorkspaceNotice.Event? {
        guard case .uploaded(let id, .failure(let error)) = action,
              diff.pendingSubmission != nil,
              diff.attachmentPicker.entries.first(where: { $0.id == id })?.status == .uploading else { return nil }
        return .failure(couldNotSendMessage(error.message))
    }

    private func rejectedPRNotice(_ outcome: PRMutationOutcome) -> WorkspaceNotice.Event? {
        guard case .rejected(let message) = outcome else { return nil }
        return .failure(couldNotSendMessage(message))
    }

    private func questionNotice(_ question: QuestionFeature.State, action: QuestionFeature.Action) -> WorkspaceNotice.Event? {
        switch action {
        case .attachmentPicker(.uploaded(let id, .failure(let error))):
            guard question.pendingSubmission != nil && question.pendingAttachedAnswer == nil,
                  question.attachmentPicker.entries.first(where: { $0.id == id })?.status == .uploading else { return nil }
            return .failure(couldNotSendMessage(error.message))
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
        case .attachmentPicker(.uploaded(let id, .failure(let error))):
            guard node.pendingSubmission != nil,
                  node.attachmentPicker.entries.first(where: { $0.id == id })?.status == .uploading else { return nil }
            return .failure(couldNotSendMessage(error.message))
        case .sendFinished(_, let outcome, let epoch):
            guard node.isSending, epoch == node.presentationEpoch else { return nil }
            switch outcome {
            case .confirmed: return .messageSent(node.node?.displayName ?? node.sessionID)
            case .rejected(let message): return .failure(couldNotSendMessage(message))
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
    public struct State: Equatable, Identifiable {
        public var id: UInt64 { pr.id }
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
