import AlondraQuestionsFeature
import AlondraPullRequestsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

/// One scene-owned draft per inbox card. Every mutation uses the existing feature use cases.
@Reducer
public struct TriageCardFeature {
    public init() {}
    public enum Resolution: Equatable {
        case answer(String?), sent, contextRequested, commentsSent, approved, changesRequested, merged, conflictsSent, setAside
    }
    @ObservableState
    public struct State: Equatable, Identifiable {
        public var id: String { item.id }
        public var item: InboxItem
        public var question: QuestionFeature.State?
        public var document: DocumentReviewFeature.State?
        public var pullRequest: ServicePullRequestFeature.State?
        public var askingContext = false
        public var contextNote = ""
        public var showNodeContext = false
        public var stamp: Resolution?
        public var pendingStamp: Resolution?
        public var requestingChanges = false
        public init(item: InboxItem, identity: QuestionWorkIdentity?) {
            self.item = item
            switch item {
            case .question(let value): question = QuestionFeature.State(question: value)
            case .report(let value): document = DocumentReviewFeature.State(subject: .report(value), identity: identity)
            case .pullRequest(let value): pullRequest = ServicePullRequestFeature.State(pr: value)
            }
        }
        public var reply: String { question?.draft.text ?? document?.draft.general ?? pullRequest?.diff.note ?? "" }
        public var selectedOption: QuestionOption? {
            guard let question else { return nil }
            return question.question.options.first { $0.label == question.draft.selectedOption }
        }
        public var canSend: Bool {
            let hasAttachments = !(question?.attachmentPicker.entries.isEmpty ?? document?.attachmentPicker.entries.isEmpty ?? pullRequest?.diff.attachmentPicker.entries.isEmpty ?? true)
            return !busy && (!reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || hasAttachments)
        }
        public var busy: Bool {
            stamp != nil || question?.isSubmitting == true || question?.needsReconciliation == true
                || document?.isSubmitting == true || document?.needsReconciliation == true
                || pullRequest?.diff.isSending == true || pullRequest?.pr.isSending == true
        }
        public var notice: String? { question?.notice ?? question?.conflict ?? document?.notice ?? document?.conflict ?? pullRequest?.diff.validationError }
    }
    public enum Action {
        case question(QuestionFeature.Action)
        case document(DocumentReviewFeature.Action)
        case pullRequest(ServicePullRequestFeature.Action)
        case refreshed(InboxItem)
        case appeared
        case replyChanged(String)
        case choose(QuestionOption)
        case clearOption
        case send
        case ask
        case contextPresented(Bool)
        case contextChanged(String)
        case requestContext
        case toggleNodeContext
        case markRead
        case stampFinished
        case delegate(Delegate)
        public enum Delegate { case resolved(String), hide(String) }
    }
    @Dependency(\.continuousClock) var clock
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .refreshed(let item):
                state.item = item
                switch item {
                case .question(let value): return .send(.question(.refreshed(value)))
                case .report(let value): return .send(.document(.refreshed(.report(value))))
                case .pullRequest(let value): return .send(.pullRequest(.updated(value)))
                }
            case .appeared:
                if state.document != nil { return .send(.document(.appeared)) }
                if state.pullRequest != nil { return .send(.pullRequest(.diff(.appeared))) }
                return .none
            case .replyChanged(let text):
                if state.question != nil { return .send(.question(.textChanged(text))) }
                if state.document != nil { return .send(.document(.generalChanged(text))) }
                return .send(.pullRequest(.diff(.noteChanged(text))))
            case .choose(let option):
                guard !state.busy else { return .none }
                state.pendingStamp = .answer(option.label)
                return .send(.question(.optionTapped(option.label)))
            case .clearOption: return .send(.question(.optionSelected(nil)))
            case .send:
                guard !state.busy else { return .none }
                if let question = state.question { state.pendingStamp = .answer(question.draft.selectedOption); return .send(.question(.answerTapped)) }
                if state.document != nil { state.pendingStamp = .sent; return .send(.document(.sendFeedbackTapped(author: "David"))) }
                state.pendingStamp = .changesRequested
                state.requestingChanges = true
                return .send(.pullRequest(.diff(.requestChangesTapped)))
            case .ask: state.requestingChanges = false; return .send(.pullRequest(.diff(.askTapped)))
            case .contextPresented(let shown): state.askingContext = shown; return .none
            case .contextChanged(let text): state.contextNote = text; return .none
            case .requestContext:
                state.askingContext = false
                state.pendingStamp = .contextRequested
                return .concatenate(.send(.question(.textChanged(state.contextNote))), .send(.question(.moreContextTapped)))
            case .toggleNodeContext: state.showNodeContext.toggle(); return .none
            case .markRead: return .send(.delegate(.hide(state.id)))
            case .stampFinished: return .send(.delegate(.resolved(state.id)))
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

public func needsText(_ option: QuestionOption) -> Bool {
    if let description = option.description, ["write", "escribe"].contains(where: { description.lowercased().hasPrefix($0) }) { return true }
    return [AnswerOption.requestChanges, AnswerOption.deny].contains(option.label)
}
