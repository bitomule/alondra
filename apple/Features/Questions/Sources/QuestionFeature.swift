import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

/// Answers one question or action request through `answer_question`, or asks its Agent for
/// more context. The option ID and the option's declared inputs are the whole contract: no
/// label or description decides anything.
@Reducer
public struct QuestionFeature {
    public enum PendingSubmission: Equatable, Sendable { case answer, moreContext }

    @ObservableState
    public struct State: Equatable {
        public var downloads = AttachmentDownloadFeature.State()
        public var attachmentPicker = AttachmentFeature.State()
        public var question: DomainQuestion
        public var workspace: DomainWorkspace
        public var draft = QuestionDraft()
        public var isSubmitting = false
        /// Waiting for attachment uploads before sending; never a retry of a sent command.
        public var pendingSubmission: PendingSubmission?
        public var keyboardReady = false
        public var notice: String?
        /// Terminal command rejections use the original workspace toast; validation and recovery stay inline.
        public var noticeIsSubmissionFailure = false
        public var conflict: String?
        public var completed = false
        /// The command ID of an attempt whose outcome is unknown. Sending again reuses it, so the
        /// server applies the answer once; editing the draft starts a new command.
        public var commandID: String?
        public var commandSubmission: PendingSubmission?
        public var needsReconciliation = false
        public var revision: UInt64 = 0
        public var replyFocusRequest: UInt64 = 0

        /// Drops only an upload-waiting gesture; drafts, receipts and dispatched commands remain intact.
        public mutating func cancelPendingSubmission() { pendingSubmission = nil }

        public init(question: DomainQuestion, workspace: DomainWorkspace) {
            self.question = question
            self.workspace = workspace
        }

        public var selectedOption: DomainV2.QuestionOption? { question.option(draft.selectedOption) }
    }

    public enum Action {
        case keyboardAppeared, keyboardDisappeared, keyboardReady
        case downloads(AttachmentDownloadFeature.Action)
        case attachmentPicker(AttachmentFeature.Action)
        case refreshed(DomainQuestion, DomainWorkspace)
        case identityChanged, disappeared
        case textChanged(String)
        case optionSelected(String?)
        case optionTapped(String)
        case answerTapped
        case moreContextTapped
        case submissionFinished(UInt64, DomainCommandResult)
        case dismissNotice
    }

    @Dependency(\.continuousClock) var clock
    @Dependency(\.domainWorkspace) var client
    @Dependency(\.uuid) var uuid

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
                if action.invalidatesPendingSubmission {
                    state.cancelPendingSubmission()
                    startNewCommand(&state)
                    return .none
                }
                guard let pending = state.pendingSubmission else { return .none }
                if case .uploaded(_, .failure(let error)) = action {
                    state.cancelPendingSubmission()
                    state.noticeIsSubmissionFailure = true
                    state.notice = error.message
                    return .none
                }
                // Earlier failed entries are expected until prepare requeues them; only a
                // new failure cancels the intent.
                guard state.attachmentPicker.allReady else { return .none }
                state.cancelPendingSubmission()
                return submit(&state, pending)
            case let .refreshed(question, workspace):
                guard state.question.id == question.id else {
                    let revision = state.revision &+ 1
                    state = State(question: question, workspace: workspace)
                    state.revision = revision
                    return .cancel(id: CancelID.submit)
                }
                // A refresh keeps the draft; an option the server no longer offers is cleared.
                if state.question != question { state.cancelPendingSubmission() }
                state.question = question
                state.workspace = workspace
                if question.option(state.draft.selectedOption) == nil { state.draft.selectedOption = nil }
                return .none
            case .disappeared:
                state.cancelPendingSubmission()
                return .none
            case .identityChanged:
                state.attachmentPicker = AttachmentFeature.State()
                state.draft = QuestionDraft()
                state.cancelPendingSubmission()
                state.isSubmitting = false
                state.noticeIsSubmissionFailure = false
                state.notice = nil
                state.conflict = nil
                state.completed = false
                state.commandID = nil
                state.needsReconciliation = false
                state.revision &+= 1
                return .cancel(id: CancelID.submit)
            case .textChanged(let text):
                guard !state.isSubmitting else { return .none }
                state.cancelPendingSubmission()
                if state.draft.text != text { startNewCommand(&state) }
                state.draft.text = text
                return .none
            case .optionSelected(let option):
                guard !state.isSubmitting else { return .none }
                state.cancelPendingSubmission()
                if state.draft.selectedOption != option { startNewCommand(&state) }
                state.draft.selectedOption = option
                return .none
            case .optionTapped(let id):
                guard !state.isSubmitting, !state.completed, state.pendingSubmission == nil,
                      let option = state.question.option(id) else { return .none }
                if state.draft.selectedOption != option.id { startNewCommand(&state) }
                state.draft.selectedOption = option.id
                if AnswerForm(option: option).requiresText,
                   state.draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    state.replyFocusRequest &+= 1
                    return .none
                }
                return prepare(&state, .answer)
            case .answerTapped:
                return prepare(&state, .answer)
            case .moreContextTapped:
                return prepare(&state, .moreContext)
            case .submissionFinished(let revision, let result):
                guard state.revision == revision, state.isSubmitting else { return .none }
                state.isSubmitting = false
                if result != .uncertain { state.commandID = nil; state.needsReconciliation = false }
                switch result {
                case .recorded:
                    state.completed = true
                    state.draft = QuestionDraft()
                    state.attachmentPicker = AttachmentFeature.State()
                case .rejected(let rejection):
                    if rejection.errorCode == .staleRevision {
                        state.conflict = rejection.localizedMessage
                    } else {
                        state.notice = rejection.localizedMessage
                    }
                    state.noticeIsSubmissionFailure = true
                case .uncertain:
                    state.noticeIsSubmissionFailure = false
                    state.notice = String(localized: "We couldn’t confirm the result. Your draft is kept; sending it again won’t apply it twice.")
                    state.needsReconciliation = true
                case .obsolete:
                    break
                }
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

    /// Uploads finish before the command is built, so the command carries every attachment.
    private func prepare(_ state: inout State, _ submission: PendingSubmission) -> Effect<Action> {
        guard !state.isSubmitting, !state.completed, state.pendingSubmission == nil,
              state.attachmentPicker.canPrepareSubmission else { return .none }
        if !state.attachmentPicker.allReady {
            state.pendingSubmission = submission
            return .send(.attachmentPicker(.prepareForSubmission))
        }
        return submit(&state, submission)
    }

    private func submit(_ state: inout State, _ submission: PendingSubmission) -> Effect<Action> {
        let attachments = state.attachmentPicker.stored
        let intent: DomainIntent
        switch submission {
        case .answer:
            do {
                guard case let .answer(id, option, inputs, _) = try state.question.intent(for: state.draft) else { return .none }
                intent = .answer(id, optionID: option, inputs: inputs, attachments: attachments)
            } catch let error as DomainIntentError {
                if case .inputRequired = error { state.replyFocusRequest &+= 1 }
                state.noticeIsSubmissionFailure = false
                state.notice = error.formMessage
                return .none
            } catch { return .none }
        case .moreContext:
            intent = .requestMoreContext(state.question.id, body: state.draft.text, attachments: attachments)
        }
        state.isSubmitting = true
        state.noticeIsSubmissionFailure = false
        state.notice = nil
        state.conflict = nil
        let revision = state.revision
        let workspace = state.workspace
        if state.commandSubmission != submission { state.commandID = nil }
        let commandID = state.commandID ?? uuid().uuidString.lowercased()
        state.commandID = commandID
        state.commandSubmission = submission
        return .run { send in
            await send(.submissionFinished(revision, await client.perform(intent, workspace, [], commandID)))
        }.cancellable(id: CancelID.submit)
    }

    /// A changed draft is a different command; an unknown earlier attempt is left as it is.
    private func startNewCommand(_ state: inout State) {
        state.commandID = nil
        state.needsReconciliation = false
    }

    private enum CancelID { case keyboard, submit }
}
