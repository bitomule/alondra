import AlondraAttachmentsFeature
import AlondraConnectionFeature
import AlondraContracts
import AlondraDomain
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraPullRequestsFeature
import AlondraQuestionsFeature
import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import Foundation

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
        public var deliveryJournal = DeliveryJournalFeature.State()
        public var macError: WorkspaceNotice?
        var lastMacErrorMessage: String?
        var macErrorMessage: String? {
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
        public var pullRequestID: DomainV2.PrId?
        public var pullRequestIdentity: TriageIdentity?
        public var pullRequest: ServicePullRequestFeature.State? {
            get { pullRequestID.flatMap { pullRequests[id: $0] } }
            set {
                guard let value = newValue else { pullRequestID = nil; return }
                pullRequests[id: value.id] = value
                pullRequestID = value.id
            }
        }
        public var pendingInboxID: String?
        public var workspace: DomainWorkspace? { triage.workspace }
        public var macSubtitle: String {
            if let problem = triage.triage.connection.problem { return String(localized: problem.title) }
            guard triage.triage.hasSnapshot, let workspace else {
                return String(localized: "Connecting to the fleet…")
            }
            let count = triage.triage.queue.count
            let pending = count == 0 ? String(localized: "All caught up") : String(localized: "\(count) pending")
            return [pending, String(localized: "\(workspace.projection.agents.count) nodes")].joined(separator: " · ")
        }
        public func inboxCount(pullRequests: Bool) -> Int {
            triage.triage.queue.count { $0.isPullRequest == pullRequests }
        }
        public func asksOnPullRequest(_ item: DomainV2.InboxItem) -> Bool {
            guard item.isPullRequest, let workspace else { return false }
            return DomainPullRequestCard(root: item, in: workspace)?.questions.isEmpty == false
        }
        /// What a sidebar row shows besides the title: who, when, and the PR it stacks on.
        public func rowContext(_ item: DomainV2.InboxItem) -> (agent: String?, date: Int64?, stackedOn: String?) {
            let pr: DomainV2.PrId? = if case .pullRequest(let id) = item.subject { id } else { nil }
            let stack = workspace.flatMap { w in w.pullRequest(pr).flatMap(w.stackedOn) }
            return (workspace?.agentName(of: item), workspace?.createdAt(of: item), stack?.stackedOnLabel)
        }
        public func taskTitle(forInboxID id: String) -> String? {
            guard let item = triage.triage.items.first(where: { $0.id == id }) else { return nil }
            return triage.task(for: item)?.title
        }
        /// The inbox item that opens this PR's detail.
        public static func itemID(of pr: DomainV2.PrId) -> String { "pr:\(pr.rawValue)" }
    }
    public enum Action {
        case appeared, disappeared
        case macPresentation(MacPresentationFeature.Action)
        case deliveryJournal(DeliveryJournalFeature.Action)
        case dismissMacError(UUID)
        case dismissNotice(UUID)
        case dismissCloseRefusal
        case tabSelected(State.Tab)
        case settingsPresented(Bool)
        case focusInbox(String)
        case triage(ServiceTriageFeature.Action)
        case tasks(ServiceTasksFeature.Action)
        case fleet(ServiceFleetFeature.Action)
        case newRequest(NewRequestFeature.Action)
        case pullRequests(IdentifiedActionOf<ServicePullRequestFeature>)
        case pullRequestDismissed(DomainV2.PrId)
    }
    @Dependency(\.continuousClock) var noticeClock
    @Dependency(\.domainWorkspace) var domain
    private enum NoticeCancelID { case timeout, macTimeout }
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
                    state.fleet.cancelPendingSubmissions()
                    state.pullRequest?.cancelPendingSubmission()
                    if let id = state.pullRequestID, state.macPresentation.selection != .item(State.itemID(of: id)) {
                        state.pullRequestID = nil
                    }
                    return .none
                }
            }
        Scope(state: \.deliveryJournal, action: \.deliveryJournal) { DeliveryJournalFeature() }
        Scope(state: \.triage, action: \.triage) { ServiceTriageFeature() }
        Scope(state: \.tasks, action: \.tasks) { ServiceTasksFeature() }
        Scope(state: \.fleet, action: \.fleet) { ServiceFleetFeature() }
        Scope(state: \.newRequest, action: \.newRequest) { NewRequestFeature() }
        Reduce { state, action in
            // Remove unavailable background owners only inside the forEach lifetime boundary.
            let selectedPRID = state.pullRequestID
            state.pullRequests.removeAll { $0.id != selectedPRID && $0.pr.pr == nil && !$0.hasOutstandingCommand }
            // Triage owns the scoped sync connection; the view alone cannot express offline.
            state.tasks.connection = state.triage.triage.connection
            switch action {
            case .appeared:
                return .merge(.send(.deliveryJournal(.appeared)), .send(.triage(.triage(.appeared))), open(.inbox))
            case .disappeared:
                state.triage.cancelPendingSubmissions()
                state.fleet.cancelPendingSubmissions()
                state.pullRequest?.cancelPendingSubmission()
                return .merge(.send(.deliveryJournal(.disappeared)), .send(.triage(.triage(.disappeared))))
            case .tabSelected(let tab):
                if state.tab != tab {
                    state.triage.cancelPendingSubmissions()
                    state.fleet.cancelPendingSubmissions()
                    state.pullRequest?.cancelPendingSubmission()
                }
                state.tab = tab
                switch tab {
                case .tasks: return open(.tasks)
                case .fleet: return open(.fleet)
                case .newRequest: return open(.newRequest)
                case .inbox, .account: return .none
                }
            case .settingsPresented(let value): state.settingsPresented = value; return .none
            case .focusInbox(let id):
                state.triage.cancelPendingSubmissions()
                state.fleet.cancelPendingSubmissions()
                state.pullRequest?.cancelPendingSubmission()
                state.tab = .inbox
                if let current = state.pullRequestID, id != State.itemID(of: current) {
                    state.pullRequestID = nil
                    if let detail = state.pullRequests[id: current], detail.pr.pr == nil, !detail.hasOutstandingCommand {
                        state.pullRequests.remove(id: current)
                    }
                }
                state.pendingInboxID = id
                return routePending(&state)
            case .triage(.triage(.identityChanged(let identity))):
                if state.pullRequestIdentity != identity {
                    state.pullRequestIdentity = identity
                    state.pullRequestID = nil
                    state.pullRequests = []
                }
                // Run removal through the owning reducer so forEach observes the removed
                // IDs and cancels their scoped effects in this same identity transition.
                return .merge(ServiceFleetFeature().reduce(into: &state.fleet, action: .identityChanged(identity)).map(Action.fleet),
                              .send(.newRequest(.identityChanged)))
            case .triage(.triage(.workspaceUpdated(let update))):
                let hidden = state.triage.triage.hiddenIDs
                guard update.identity == state.triage.triage.identity else { return routePending(&state) }
                let workspace = update.workspace
                var updates: [Effect<Action>] = [
                    routePending(&state),
                    .send(.newRequest(.workspaceUpdated(workspace))),
                    .send(.tasks(.workspaceChanged(workspace))), .send(.tasks(.hiddenInboxChanged(hidden))),
                    .send(.fleet(.workspaceChanged(workspace))), .send(.fleet(.hiddenInboxChanged(hidden))),
                ]
                if let revision = workspace?.revision { updates.append(.send(.deliveryJournal(.syncTick(revision)))) }
                for detail in state.pullRequests {
                    let id = detail.id
                    guard let workspace else {
                        updates.append(.send(.pullRequests(.element(id: id, action: .snapshotUnavailable))))
                        continue
                    }
                    let card = workspace.item(State.itemID(of: id)).flatMap { DomainPullRequestCard(root: $0, in: workspace) }
                    if card == nil, state.pullRequestID != id, !detail.hasOutstandingCommand {
                        state.pullRequests.remove(id: id)
                        continue
                    }
                    if card == nil { state.pullRequests[id: id]?.cancelPendingSubmission() }
                    updates.append(.send(.pullRequests(.element(id: id, action: .updated(card, workspace)))))
                }
                return .merge(updates)
            case .triage(.delegate(.pullRequest(let card))):
                guard let workspace = state.workspace else { return .none }
                state.pullRequestIdentity = state.triage.triage.identity
                state.pullRequest?.cancelPendingSubmission()
                if state.pullRequests[id: card.id] == nil {
                    state.pullRequest = ServicePullRequestFeature.State(card: card, workspace: workspace)
                } else {
                    state.pullRequestID = card.id
                    if state.pullRequests[id: card.id]?.diff.shouldDismiss == true {
                        state.pullRequests[id: card.id]?.diff = DiffReviewFeature.State(card: card, workspace: workspace)
                    }
                }
                return .merge(.send(.pullRequests(.element(id: card.id, action: .updated(card, workspace)))), open(.pullRequest))
            case .pullRequestDismissed(let id):
                guard state.pullRequestID == id else { return .none }
                state.pullRequest?.cancelPendingSubmission()
                state.pullRequestID = nil
                if let detail = state.pullRequests[id: id], detail.pr.pr == nil, !detail.hasOutstandingCommand {
                    state.pullRequests.remove(id: id)
                }
                return .none
            case .pullRequests(.element(let id, .pr(.mutationFinished))), .pullRequests(.element(let id, .diff(.reviewFinished))):
                if let detail = state.pullRequests[id: id], detail.pr.shouldDismiss || detail.diff.shouldDismiss,
                   state.pullRequestID == id {
                    state.pullRequestID = nil
                }
                return .none
            case .newRequest(.appeared): return .send(.newRequest(.availabilityChanged(state.newRequest.canSubmit)))
            case .triage(.machinePresented(true)):
                return open(.machine)
            case .triage:
                let hidden = state.triage.triage.hiddenIDs
                return .merge(.send(.fleet(.hiddenInboxChanged(hidden))), .send(.tasks(.hiddenInboxChanged(hidden))))
            case .macPresentation, .dismissMacError, .deliveryJournal, .tasks, .fleet, .newRequest, .pullRequests,
                 .dismissNotice, .dismissCloseRefusal: return .none
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

    private func open(_ screen: DomainScreen) -> Effect<Action> {
        .run { [domain] _ in await domain.open(screen) }
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
        case .triage(.documents(.element(let id, let action))):
            return state.triage.documents[id: id].flatMap { documentNotice($0, action: action) }
        case .pullRequests(.element(let id, .diff(.reviewFinished(let outcome, let epoch)))):
            guard let diff = state.pullRequests[id: id]?.diff, diff.isSending, diff.presentationEpoch == epoch else { return nil }
            return rejectedPRNotice(outcome)
        case .pullRequests(.element(let id, .pr(.mutationFinished(let outcome, let epoch)))):
            guard let pr = state.pullRequests[id: id]?.pr, pr.isSending, pr.presentationEpoch == epoch else { return nil }
            return rejectedPRNotice(outcome)
        case .tasks(.board(.archiveFinished(let outcomes, let epoch))):
            guard state.tasks.board.isArchiving, state.tasks.board.presentationEpoch == epoch else { return nil }
            return outcomes.values.lazy.compactMap(rejectedTaskNotice).first
        case .tasks(.withdrawFinished(let id, let outcome)):
            guard state.tasks.isWithdrawing, state.tasks.withdrawOperationRequestID == id else { return nil }
            return rejectedTaskNotice(outcome)
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
        case .fleet(.fleet(.closeFinished(let id, let outcome, let epoch))):
            let fleet = state.fleet.fleet
            guard fleet.presentationEpoch == epoch, fleet.closingIDs.contains(id) else { return nil }
            switch outcome {
            case .confirmed: return .closed(fleet.snapshot?.node(id)?.node.displayName ?? id)
            case .rejected(let message): return .closeRefused(message)
            default: return nil
            }
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
        guard case .mutationFinished(let outcome, let epoch) = action, task.isActing, task.presentationEpoch == epoch else { return nil }
        return rejectedTaskNotice(outcome)
    }

    private func rejected(_ result: DomainCommandResult) -> WorkspaceNotice.Event? {
        guard case .rejected(let rejection) = result else { return nil }
        return .failure(couldNotSendMessage(rejection.localizedMessage))
    }

    private func documentNotice(_ document: DocumentReviewFeature.State, action: DocumentReviewFeature.Action) -> WorkspaceNotice.Event? {
        switch action {
        case .attachmentPicker(.uploaded(let id, .failure(let error))):
            guard document.pendingReview != nil,
                  document.attachmentPicker.entries.first(where: { $0.id == id })?.status == .uploading else { return nil }
            return .failure(couldNotSendMessage(error.message))
        case .submissionFinished(let revision, let result):
            guard document.isSubmitting, document.revision == revision else { return nil }
            return rejected(result)
        default: return nil
        }
    }

    private func diffUploadNotice(_ diff: DiffReviewFeature.State, action: AttachmentFeature.Action) -> WorkspaceNotice.Event? {
        guard case .uploaded(let id, .failure(let error)) = action,
              diff.pendingReviewKind != nil,
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
            guard question.pendingSubmission != nil,
                  question.attachmentPicker.entries.first(where: { $0.id == id })?.status == .uploading else { return nil }
            return .failure(couldNotSendMessage(error.message))
        case .submissionFinished(let revision, let result):
            guard question.isSubmitting, question.revision == revision else { return nil }
            return rejected(result)
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
