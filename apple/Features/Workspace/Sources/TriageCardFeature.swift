import AlondraContracts
import AlondraDomain
import AlondraQuestionsFeature
import AlondraPullRequestsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

/// What a card opens: the interaction behind it, the delivery inside a document review, or the
/// pull request with its child questions. The server grouped them; this only follows IDs.
public enum CardContent: Equatable, Sendable {
    case question(DomainQuestion)
    case document(DocumentSubject)
    case pullRequest(DomainPullRequestCard)

    public init?(_ item: DomainV2.InboxItem, in workspace: DomainWorkspace) {
        switch item.subject {
        case .pullRequest:
            guard let card = DomainPullRequestCard(root: item, in: workspace) else { return nil }
            self = .pullRequest(card)
        case .documentReview:
            let children = item.children.filter { workspace.interaction(of: $0) != nil }
            let target = children.first { child in
                if case .delivery = workspace.interaction(of: child)?.payload { return child.pending }
                return false
            } ?? children.first(where: \.pending) ?? children.first
            guard let target, let subject = DocumentSubject(item: target, in: workspace) else { return nil }
            self = .document(subject)
        case .interaction:
            if let question = DomainQuestion(item: item, in: workspace) {
                self = .question(question)
            } else if let subject = DocumentSubject(item: item, in: workspace) {
                self = .document(subject)
            } else {
                return nil
            }
        }
    }
}

/// One scene-owned draft per inbox card. Every mutation uses the existing feature use cases.
@Reducer
public struct TriageCardFeature {
    public init() {}
    public enum Resolution: Equatable {
        case answer(String?), read, sent, contextRequested, commentsSent, approved, changesRequested, merged, conflictsSent, setAside
    }
    @ObservableState
    public struct State: Equatable, Identifiable {
        public var id: String { item.id }
        public var item: DomainV2.InboxItem
        public var question: QuestionFeature.State?
        public var document: DocumentReviewFeature.State?
        public var pullRequest: ServicePullRequestFeature.State?
        public var askingContext = false
        public var contextNote = ""
        public var showNodeContext = false
        public var stamp: Resolution?
        public var pendingStamp: Resolution?
        public var readPending = false
        public var requestingChanges = false
        public init(item: DomainV2.InboxItem, workspace: DomainWorkspace, identity: QuestionWorkIdentity?) {
            self.item = item
            switch CardContent(item, in: workspace) {
            case .question(let value): question = QuestionFeature.State(question: value, workspace: workspace)
            case .document(let value): document = DocumentReviewFeature.State(subject: value, workspace: workspace, identity: identity)
            case .pullRequest(let value): pullRequest = ServicePullRequestFeature.State(card: value, workspace: workspace)
            case nil: break
            }
        }
        public mutating func cancelPendingSubmission() {
            question?.cancelPendingSubmission()
            document?.cancelPendingSubmission()
            pullRequest?.cancelPendingSubmission()
        }
        public var reply: String { question?.draft.text ?? document?.draft.general ?? pullRequest?.diff.note ?? "" }
        public var selectedOption: DomainV2.QuestionOption? { question?.selectedOption }
        public var canMarkRead: Bool { item.availableActions.contains { $0.actionKind == .markRead && $0.enabled } }
        public var canSend: Bool {
            let hasAttachments = !(question?.attachmentPicker.entries.isEmpty ?? document?.attachmentPicker.entries.isEmpty ?? pullRequest?.diff.attachmentPicker.entries.isEmpty ?? true)
            return !busy && reviewReady && (!reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || hasAttachments)
        }
        /// A document or diff that is still loading, or failed to load, cannot take a review yet.
        public var reviewReady: Bool { document?.canReview ?? pullRequest?.diff.canReview ?? true }
        public var retainsPendingDelivery: Bool {
            question?.isSubmitting == true || question?.needsReconciliation == true
                || document?.isSubmitting == true || document?.needsReconciliation == true
                || pullRequest?.hasOutstandingCommand == true
        }
        public var busy: Bool {
            stamp != nil || readPending || question?.isSubmitting == true
                || document?.isSubmitting == true
                || pullRequest?.diff.isSending == true || pullRequest?.pr.isSending == true
        }
        public var notice: String? {
            let questionNotice = question?.noticeIsSubmissionFailure == true ? nil : (question?.notice ?? question?.conflict)
            let documentNotice = document?.noticeIsSubmissionFailure == true ? nil : (document?.notice ?? document?.conflict)
            let diffNotice = pullRequest?.diff.validationErrorIsSubmissionFailure == true ? nil : pullRequest?.diff.validationError
            return questionNotice ?? documentNotice ?? diffNotice
        }
    }
    public enum Action {
        case question(QuestionFeature.Action)
        case document(DocumentReviewFeature.Action)
        case pullRequest(ServicePullRequestFeature.Action)
        case refreshed(DomainV2.InboxItem, DomainWorkspace)
        case appeared
        case replyChanged(String)
        case choose(DomainV2.QuestionOption)
        case clearOption
        case send
        case ask
        case contextPresented(Bool)
        case contextChanged(String)
        case requestContext
        case toggleNodeContext
        case markRead
        case readAcknowledged(Bool)
        case stampFinished
        case delegate(Delegate)
        public enum Delegate { case resolved(String), hide(String), read(String), readPresentationFinished(String) }
    }
    @Dependency(\.continuousClock) var clock
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .refreshed(let item, let workspace):
                state.item = item
                switch CardContent(item, in: workspace) {
                case .question(let value):
                    guard state.question != nil else {
                        state.question = QuestionFeature.State(question: value, workspace: workspace)
                        return .none
                    }
                    return .send(.question(.refreshed(value, workspace)))
                case .document(let value):
                    guard state.document != nil else {
                        state.document = DocumentReviewFeature.State(subject: value, workspace: workspace)
                        return .none
                    }
                    return .send(.document(.refreshed(value, workspace)))
                case .pullRequest(let value):
                    guard state.pullRequest != nil else {
                        state.pullRequest = ServicePullRequestFeature.State(card: value, workspace: workspace)
                        return .none
                    }
                    return .send(.pullRequest(.updated(value, workspace)))
                case nil: return .none
                }
            case .appeared:
                if state.document != nil { return .send(.document(.phoneAppeared)) }
                if state.pullRequest != nil { return .send(.pullRequest(.diff(.appeared))) }
                return .none
            case .replyChanged(let text):
                if state.question != nil { return .send(.question(.textChanged(text))) }
                if state.document != nil { return .send(.document(.generalChanged(text))) }
                return .send(.pullRequest(.diff(.noteChanged(text))))
            case .choose(let option):
                guard !state.busy else { return .none }
                state.pendingStamp = .answer(option.label)
                return .send(.question(.optionTapped(option.id)))
            case .clearOption: return .send(.question(.optionSelected(nil)))
            case .send:
                guard !state.busy, state.reviewReady else { return .none }
                if let question = state.question {
                    state.pendingStamp = .answer(question.selectedOption?.label)
                    return .send(.question(.answerTapped))
                }
                if state.document != nil { state.pendingStamp = .sent; return .send(.document(.sendFeedbackTapped(author: state.document?.feedbackAuthor ?? ""))) }
                state.pendingStamp = .changesRequested
                state.requestingChanges = true
                return .send(.pullRequest(.diff(.requestChangesTapped)))
            case .ask:
                guard state.reviewReady else { return .none }
                state.requestingChanges = false
                return .send(.pullRequest(.diff(.askTapped)))
            case .contextPresented(let shown): state.askingContext = shown; return .none
            case .contextChanged(let text): state.contextNote = text; return .none
            case .requestContext:
                state.askingContext = false
                state.pendingStamp = .contextRequested
                if state.document != nil {
                    return .concatenate(.send(.document(.generalChanged(state.contextNote))),
                                        .send(.document(.moreContextTapped(author: state.document?.feedbackAuthor ?? ""))))
                }
                return .concatenate(.send(.question(.textChanged(state.contextNote))), .send(.question(.moreContextTapped)))
            case .toggleNodeContext: state.showNodeContext.toggle(); return .none
            case .markRead:
                guard !state.busy, state.canMarkRead else { return .none }
                state.readPending = true
                return .send(.delegate(.read(state.id)))
            case .readAcknowledged(let confirmed):
                guard state.readPending else { return .none }
                state.readPending = false
                guard confirmed else { return .none }
                state.stamp = .read
                return .run { send in
                    try await clock.sleep(for: .milliseconds(520))
                    await send(.stampFinished)
                }
            case .stampFinished:
                guard state.stamp != nil else { return .none }
                return .send(state.stamp == .read ? .delegate(.readPresentationFinished(state.id)) : .delegate(.resolved(state.id)))
            case .document(.reviewTapped(let verdict)):
                state.pendingStamp = verdict == .approved ? .approved : .commentsSent
                return .none
            case .pullRequest(.pr(.confirmMerge)):
                state.pendingStamp = .merged
                return .none
            case .pullRequest(.pr(.resolveConflictsTapped)):
                state.pendingStamp = .conflictsSent
                return .none
            case .pullRequest(.pr(.setAsideTapped)):
                state.pendingStamp = .setAside
                return .none
            case .question, .document, .pullRequest:
                // Asking keeps this card open. A confirmed ask must allow the next draft.
                if !state.requestingChanges, state.pullRequest?.diff.mutationOutcome == .confirmed {
                    state.pullRequest?.diff.mutationOutcome = nil
                    state.pullRequest?.diff.shouldDismiss = false
                }
                let completed = state.question?.completed == true || state.document?.completed == true
                    || state.pullRequest?.pr.mutationOutcome == .confirmed
                    || (state.pullRequest?.diff.mutationOutcome == .confirmed && state.requestingChanges)
                guard completed, state.stamp == nil else { return .none }
                state.stamp = state.pendingStamp ?? .sent
                return .run { send in
                    try await clock.sleep(for: .milliseconds(520))
                    await send(.stampFinished)
                }
            case .delegate: return .none
            }
        }
        .ifLet(\.question, action: \.question) { QuestionFeature() }
        .ifLet(\.document, action: \.document) { DocumentReviewFeature() }
        .ifLet(\.pullRequest, action: \.pullRequest) { ServicePullRequestFeature() }
    }
}

/// An option whose input is required text needs the reply field before it can be sent.
public func needsText(_ option: DomainV2.QuestionOption) -> Bool {
    option.inputs.contains { $0.kind == .text && $0.required }
}
