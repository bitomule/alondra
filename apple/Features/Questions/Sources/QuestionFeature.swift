import AlondraDomain
import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

@Reducer
public struct QuestionFeature {
    @ObservableState
    public struct State: Equatable {
        public var downloads = AttachmentDownloadFeature.State()
        public var attachmentPicker = AttachmentFeature.State()
        public var question: Question
        public var draft = QuestionDraft()
        public var isSubmitting = false
        public var pendingAttachedAnswer: QuestionDraft?
        public var pendingSubmission: AttachmentSubmissionIntent?
        public var pendingMoreContext = false
        public var keyboardReady = false
        public var notice: String?
        /// Terminal command rejections use the original workspace toast; validation and recovery stay inline.
        public var noticeIsSubmissionFailure = false
        public var conflict: String?
        public var completed = false
        public var uncertainCommandID: UUID?
        public var needsReconciliation = false
        public var revision: UInt64 = 0
        public var replyFocusRequest: UInt64 = 0

        /// Drops only an upload-waiting gesture; drafts, receipts and dispatched commands remain intact.
        public mutating func cancelPendingSubmission() {
            pendingSubmission = nil
            pendingAttachedAnswer = nil
            pendingMoreContext = false
        }

        public init(question: Question) { self.question = question }
    }

    public enum Action {
        case keyboardAppeared, keyboardDisappeared, keyboardReady
        case downloads(AttachmentDownloadFeature.Action)
        case attachmentPicker(AttachmentFeature.Action)
        case refreshed(Question)
        case identityChanged, disappeared
        case textChanged(String)
        case optionSelected(String?)
        case optionTapped(String)
        case attachedOptionTapped(String)
        case attachedAnswerTapped
        case attachedSubmissionFailed(String)
        case attachmentsChanged([String])
        case answerTapped
        case moreContextTapped
        case submissionFinished(UInt64, QuestionWorkOutcome)
        /// H sends this only after command journal and snapshot reconciliation.
        case commandResolved(UUID, rejection: String?)
        case reconciled(cardStillOpen: Bool)
        case dismissNotice
    }

    @Dependency(\.continuousClock) var clock
    @Dependency(\.questionWorkService) var service
    private let useCase = AnswerQuestionUseCase()

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.downloads, action: \.downloads) { AttachmentDownloadFeature() }
        Scope(state: \.attachmentPicker, action: \.attachmentPicker) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .keyboardAppeared:
                state.keyboardReady = false
                return .run { send in
                    try await clock.sleep(for: .milliseconds(450))
                    await send(.keyboardReady)
                }.cancellable(id: CancelID.keyboard, cancelInFlight: true)
            case .keyboardDisappeared:
                state.keyboardReady = false
                return .cancel(id: CancelID.keyboard)
            case .keyboardReady:
                state.keyboardReady = true
                return .none
            case .downloads: return .none
            case .attachmentPicker(let action):
                state.draft.attachmentIDs = state.attachmentPicker.storedIDs
                if action.invalidatesPendingSubmission {
                    clearPending(&state)
                    return .none
                }
                guard state.pendingSubmission != nil else { return .none }
                if case .uploaded(_, .failure(let error)) = action {
                    let attached = state.pendingAttachedAnswer != nil
                    clearPending(&state)
                    if attached { return .send(.attachedSubmissionFailed(error.message)) }
                    state.noticeIsSubmissionFailure = true
                    state.notice = error.message
                    return .none
                }
                // The prepare action is asynchronous; pre-existing failed entries are
                // expected until it requeues them. Only a new failure cancels this intent.
                guard state.attachmentPicker.allReady else { return .none }
                let attached = state.pendingAttachedAnswer
                let moreContext = state.pendingMoreContext
                clearPending(&state)
                if var draft = attached {
                    draft.attachmentIDs = state.attachmentPicker.storedIDs
                    return submitAttached(draft, state: &state)
                }
                return submitNormal(&state, moreContext: moreContext)
            case .refreshed(let question):
                guard state.question.id == question.id else {
                    let revision = state.revision &+ 1
                    state = State(question: question)
                    state.revision = revision
                    return .cancel(id: CancelID.submit)
                }
                // Keep local text, option and stored attachments while a live card refreshes.
                if state.question != question { clearPending(&state) }
                state.question = question
                if let option = state.draft.selectedOption,
                   !question.options.contains(where: { $0.label == option }),
                   !(question.isPlan && [AnswerOption.approve, AnswerOption.requestChanges].contains(option)) {
                    state.draft.selectedOption = nil
                }
                return .none
            case .disappeared:
                clearPending(&state)
                return .none
            case .identityChanged:
                state.attachmentPicker = AttachmentFeature.State()
                state.draft = QuestionDraft()
                clearPending(&state)
                state.isSubmitting = false
                state.noticeIsSubmissionFailure = false
                state.notice = nil
                state.conflict = nil
                state.completed = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.revision &+= 1
                return .cancel(id: CancelID.submit)
            case .textChanged(let text):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                clearPending(&state)
                state.draft.text = text
                return .none
            case .attachedSubmissionFailed(let message):
                state.noticeIsSubmissionFailure = false
                state.notice = message
                return .none
            case .attachedOptionTapped(let label):
                guard !state.isSubmitting, state.pendingSubmission == nil,
                      !state.completed, !state.needsReconciliation,
                      let option = state.question.options.first(where: { $0.label == label }) else { return .none }
                if AnswerQuestionUseCase.needsExplanation(option), state.draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    state.draft.selectedOption = label
                    state.replyFocusRequest &+= 1
                    return .none
                }
                var draft = state.draft
                draft.selectedOption = label
                // Original choose() sends the option label when no text was entered,
                // including when the answer also carries attachments.
                if draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { draft.text = label }
                return prepareAttached(draft, state: &state)
            case .attachedAnswerTapped:
                guard !state.isSubmitting, state.pendingSubmission == nil,
                      !state.completed, !state.needsReconciliation else { return .none }
                return prepareAttached(state.draft, state: &state)
            case .optionTapped(let option):
                guard state.pendingSubmission == nil, state.attachmentPicker.canPrepareSubmission, !state.isSubmitting,
                      !state.completed, !state.needsReconciliation else { return .none }
                guard state.question.options.contains(where: { $0.label == option }) else { return .none }
                state.draft.selectedOption = option
                do {
                    _ = try useCase.answer(state.question, draft: state.draft)
                    return .send(.answerTapped)
                } catch QuestionWorkError.explanationRequired {
                    state.replyFocusRequest &+= 1
                    return .none
                } catch {
                    state.noticeIsSubmissionFailure = false
                    state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription
                    return .none
                }
            case .optionSelected(let option):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                clearPending(&state)
                state.draft.selectedOption = option
                return .none
            case .attachmentsChanged(let ids):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                clearPending(&state)
                state.draft.attachmentIDs = ids
                return .none
            case .answerTapped, .moreContextTapped:
                guard state.pendingSubmission == nil, state.attachmentPicker.canPrepareSubmission,
                      !state.isSubmitting, !state.completed, !state.needsReconciliation else { return .none }
                let moreContext: Bool
                if case .moreContextTapped = action { moreContext = true } else { moreContext = false }
                if !state.attachmentPicker.allReady {
                    state.pendingSubmission = .awaitingReceipts
                    state.pendingMoreContext = moreContext
                    return .send(.attachmentPicker(.prepareForSubmission))
                }
                return submitNormal(&state, moreContext: moreContext)
            case .submissionFinished(let revision, let outcome):
                guard state.revision == revision, state.isSubmitting else { return .none }
                state.isSubmitting = false
                switch outcome {
                case .confirmed:
                    state.completed = true
                    state.draft = QuestionDraft()
                    state.attachmentPicker = AttachmentFeature.State()
                case .conflict(let reason):
                    state.conflict = reason
                    state.noticeIsSubmissionFailure = true
                case .forbidden(let reason), .failed(let reason):
                    state.notice = reason
                    state.noticeIsSubmissionFailure = true
                case .uncertainCommand(let id):
                    state.uncertainCommandID = id
                    state.noticeIsSubmissionFailure = false
                    state.notice = "The result is uncertain. Checking its status…"
                    state.needsReconciliation = true
                case .uncertain:
                    state.noticeIsSubmissionFailure = false
                    state.notice = "The result is uncertain. Check the server before trying again."
                    state.needsReconciliation = true
                case .obsolete:
                    break
                }
                return .none
            case .commandResolved(let id, let rejection):
                guard state.needsReconciliation, state.uncertainCommandID == id else { return .none }
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.notice = rejection
                state.noticeIsSubmissionFailure = rejection != nil
                if rejection == nil {
                    state.completed = true
                    state.draft = QuestionDraft()
                    state.attachmentPicker = AttachmentFeature.State()
                }
                return .none
            case .reconciled(let cardStillOpen):
                guard state.needsReconciliation else { return .none }
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.completed = !cardStillOpen
                state.noticeIsSubmissionFailure = false
                state.notice = nil
                if !cardStillOpen { state.draft = QuestionDraft() }
                return .none
            case .dismissNotice:
                state.noticeIsSubmissionFailure = false
                state.notice = nil
                state.conflict = nil
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in
                AttachmentFeature.cancelEffects(instanceID: oldID).map { .attachmentPicker($0) }
            }
        }
    }

    private func clearPending(_ state: inout State) { state.cancelPendingSubmission() }

    private func submitNormal(_ state: inout State, moreContext: Bool) -> Effect<Action> {
        do {
            let operation = try moreContext
                ? useCase.checkedMoreContext(state.question, draft: state.draft)
                : useCase.answer(state.question, draft: state.draft)
            state.isSubmitting = true
            state.noticeIsSubmissionFailure = false
            state.notice = nil
            state.conflict = nil
            let revision = state.revision
            return .run { send in
                await send(.submissionFinished(revision, await service.submit(operation)))
            }.cancellable(id: CancelID.submit)
        } catch {
            state.noticeIsSubmissionFailure = false
            state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription
            return .none
        }
    }

    private func prepareAttached(_ draft: QuestionDraft, state: inout State) -> Effect<Action> {
        if let error = state.attachmentPicker.importError { return .send(.attachedSubmissionFailed(error)) }
        guard state.attachmentPicker.canPrepareSubmission else { return .none }
        if !state.attachmentPicker.allReady {
            state.pendingAttachedAnswer = draft
            state.pendingSubmission = .awaitingReceipts
            return .send(.attachmentPicker(.prepareForSubmission))
        }
        return submitAttached(draft, state: &state)
    }

    private func submitAttached(_ draft: QuestionDraft, state: inout State) -> Effect<Action> {
        do {
            let operation = try useCase.answer(state.question, draft: draft)
            state.isSubmitting = true
            state.noticeIsSubmissionFailure = false
            state.notice = nil
            state.conflict = nil
            let revision = state.revision
            return .run { send in
                await send(.submissionFinished(revision, await service.submit(operation)))
            }.cancellable(id: CancelID.submit)
        } catch {
            return .send(.attachedSubmissionFailed((error as? QuestionWorkError)?.message ?? error.localizedDescription))
        }
    }

    private enum CancelID { case keyboard, submit }
}

/// Action cards share the answer contract but have a distinct entry point for presentation.
/// H can scope its Mac/iOS action view here without bypassing QuestionFeature's state machine.
@Reducer
public struct ActionFeature {
    @ObservableState
    public struct State: Equatable {
        public var question: QuestionFeature.State
        public init(question: Question) { self.question = QuestionFeature.State(question: question) }
    }

    public enum Action {
        case question(QuestionFeature.Action)
    }

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.question, action: \.question) { QuestionFeature() }
    }
}
