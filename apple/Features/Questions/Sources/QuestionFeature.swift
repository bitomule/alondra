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
        public var notice: String?
        public var conflict: String?
        public var completed = false
        public var uncertainCommandID: UUID?
        public var needsReconciliation = false
        public var revision: UInt64 = 0
        public var replyFocusRequest: UInt64 = 0

        public init(question: Question) { self.question = question }
    }

    public enum Action {
        case downloads(AttachmentDownloadFeature.Action)
        case attachmentPicker(AttachmentFeature.Action)
        case refreshed(Question)
        case identityChanged
        case textChanged(String)
        case optionSelected(String?)
        case optionTapped(String)
        case attachmentsChanged([String])
        case answerTapped
        case moreContextTapped
        case submissionFinished(UInt64, QuestionWorkOutcome)
        /// H sends this only after command journal and snapshot reconciliation.
        case commandResolved(UUID, rejection: String?)
        case reconciled(cardStillOpen: Bool)
        case dismissNotice
    }

    @Dependency(\.questionWorkService) var service
    private let useCase = AnswerQuestionUseCase()

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.downloads, action: \.downloads) { AttachmentDownloadFeature() }
        Scope(state: \.attachmentPicker, action: \.attachmentPicker) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .downloads: return .none
            case .attachmentPicker:
                state.draft.attachmentIDs = state.attachmentPicker.storedIDs
                return .none
            case .refreshed(let question):
                guard state.question.id == question.id else {
                    let revision = state.revision &+ 1
                    state = State(question: question)
                    state.revision = revision
                    return .cancel(id: CancelID.submit)
                }
                // Keep local text, option and stored attachments while a live card refreshes.
                state.question = question
                if let option = state.draft.selectedOption,
                   !question.options.contains(where: { $0.label == option }),
                   !(question.isPlan && [AnswerOption.approve, AnswerOption.requestChanges].contains(option)) {
                    state.draft.selectedOption = nil
                }
                return .none
            case .identityChanged:
                state.attachmentPicker = AttachmentFeature.State()
                state.draft = QuestionDraft()
                state.isSubmitting = false
                state.notice = nil
                state.conflict = nil
                state.completed = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.revision &+= 1
                return .cancel(id: CancelID.submit)
            case .textChanged(let text):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.text = text
                return .none
            case .optionTapped(let option):
                guard state.attachmentPicker.allReady, !state.isSubmitting,
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
                    state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription
                    return .none
                }
            case .optionSelected(let option):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.selectedOption = option
                return .none
            case .attachmentsChanged(let ids):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.attachmentIDs = ids
                return .none
            case .answerTapped, .moreContextTapped:
                guard state.attachmentPicker.allReady, !state.isSubmitting, !state.completed, !state.needsReconciliation else { return .none }
                let operation: QuestionOperation
                do {
                    if case .answerTapped = action {
                        operation = try useCase.answer(state.question, draft: state.draft)
                    } else {
                        operation = try useCase.checkedMoreContext(state.question, draft: state.draft)
                    }
                } catch {
                    state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription
                    return .none
                }
                state.isSubmitting = true
                state.notice = nil
                state.conflict = nil
                let revision = state.revision
                return .run { send in
                    await send(.submissionFinished(revision, await service.submit(operation)))
                }.cancellable(id: CancelID.submit)
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
                case .forbidden(let reason), .failed(let reason):
                    state.notice = reason
                case .uncertainCommand(let id):
                    state.uncertainCommandID = id
                    state.notice = "The result is uncertain. Checking its status…"
                    state.needsReconciliation = true
                case .uncertain:
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
                state.notice = nil
                if !cardStillOpen { state.draft = QuestionDraft() }
                return .none
            case .dismissNotice:
                state.notice = nil
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in
                AttachmentFeature.cancelEffects(instanceID: oldID).map { .attachmentPicker($0) }
            }
        }
    }

    private enum CancelID { case submit }
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
