import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

public enum DocumentSubject: Equatable, Sendable {
    case question(Question)
    case report(Report)

    public var id: String {
        switch self {
        case .question(let question): "q\(question.id)"
        case .report(let report): "r\(report.id)"
        }
    }

    public var attachments: [Attachment] {
        switch self {
        case .question(let question): question.attachments
        case .report(let report): report.attachments
        }
    }

    public var sources: [DocumentSource] {
        switch self {
        case .question(let question):
            let attachments = question.attachments.filter(\.isText).compactMap { attachment in
                attachment.storedPath.map { DocumentSource.stored(name: attachment.name, path: $0) }
            }
            if question.isPlan, let detail = question.detail { return [.inline(detail)] + attachments }
            let detail = question.readableDetail ?? ""
            return detail.isEmpty && !attachments.isEmpty ? attachments : [.inline(detail)] + attachments
        case .report(let report):
            let attachments = report.attachments.filter(\.isText).compactMap { attachment in
                attachment.storedPath.map { DocumentSource.stored(name: attachment.name, path: $0) }
            }
            let body = report.body ?? ""
            return body.isEmpty && !attachments.isEmpty ? attachments : [.inline(body)] + attachments
        }
    }
}

public enum DocumentSource: Equatable, Sendable {
    case inline(String)
    case stored(name: String, path: String)
}

public struct DocumentDraft: Equatable, Sendable {
    public var comments: [ReviewComment] = []
    public var general = ""
    public var selectedOption: String?
    public var attachmentIDs: [String] = []
    public init() {}
}

@Reducer
public struct DocumentReviewFeature {
    @ObservableState
    public struct State: Equatable {
        public var downloads = AttachmentDownloadFeature.State()
        public var attachmentPicker = AttachmentFeature.State()
        public var subject: DocumentSubject
        public var identity: QuestionWorkIdentity?
        public var sourceText: String?
        public var previousTextForComparison: String?
        public var isLoading = false
        public var loadError: String?
        public var draft = DocumentDraft()
        public var isSubmitting = false
        public var completed = false
        public var uncertainCommandID: UUID?
        public var feedbackReadRetryAvailable = false
        public var isRetryingFeedbackRead = false
        public var needsReconciliation = false
        public var conflict: String?
        public var notice: String?
        public var revision: UInt64 = 0
        public var loadRevision: UInt64 = 0

        public init(subject: DocumentSubject, identity: QuestionWorkIdentity? = nil) {
            self.subject = subject
            self.identity = identity
        }
    }

    public enum Action {
        case downloads(AttachmentDownloadFeature.Action)
        case attachmentPicker(AttachmentFeature.Action)
        case appeared
        case disappeared
        case refreshed(DocumentSubject)
        case identityChanged(QuestionWorkIdentity?)
        case loaded(UInt64, Result<String, DocumentLoadFailure>)
        case generalChanged(String)
        case commentChanged(anchor: String, quote: String, text: String)
        case optionSelected(String?)
        case attachmentsChanged([String])
        case sendFeedbackTapped(author: String)
        case reviewTapped(ReviewVerdict)
        /// H sends this only after journal and snapshot reconciliation, never merely on refresh.
        case feedbackReadRejected(UUID, String)
        case retryFeedbackRead
        case feedbackReadRetryFinished(UUID)
        case commandResolved(UUID, rejection: String?)
        case reconciled(cardStillOpen: Bool)
        case submissionFinished(UInt64, QuestionOperation, QuestionWorkOutcome)
        case reportFeedbackFinished(UInt64, QuestionWorkOutcome)
        case dismissNotice
    }

    @Dependency(\.questionWorkService) var service
    private let answerUseCase = AnswerQuestionUseCase()
    private let reviewUseCase = ReviewDocumentUseCase()
    private enum CancelID { case load, submit, retryRead }

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
            case .appeared:
                return load(&state)
            case .disappeared:
                return .cancel(id: CancelID.load)
            case .refreshed(let subject):
                guard state.subject.id == subject.id else {
                    let revision = state.revision &+ 1
                    let loadRevision = state.loadRevision &+ 1
                    state = State(subject: subject, identity: state.identity)
                    state.revision = revision
                    state.loadRevision = loadRevision
                    // load() replaces the prior load through cancelInFlight. Cancel submission
                    // first so an old card's acknowledgement cannot race this card's new load.
                    return .concatenate(.cancel(id: CancelID.submit), load(&state))
                }
                let oldSource = state.subject.sources
                state.subject = subject
                if oldSource != subject.sources {
                    state.draft.comments = []
                    state.notice = "The document changed. Review it again before sending comments."
                    return load(&state)
                }
                return .none
            case .identityChanged(let identity):
                state.attachmentPicker = AttachmentFeature.State()
                state.identity = identity
                state.draft = DocumentDraft()
                state.sourceText = nil
                state.previousTextForComparison = nil
                state.isLoading = false
                state.loadError = nil
                state.isSubmitting = false
                state.completed = false
                state.feedbackReadRetryAvailable = false
                state.isRetryingFeedbackRead = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.conflict = nil
                state.notice = nil
                state.revision &+= 1
                state.loadRevision &+= 1
                return .merge(.cancel(id: CancelID.load), .cancel(id: CancelID.submit), .cancel(id: CancelID.retryRead))
            case .loaded(let revision, let result):
                guard state.loadRevision == revision else { return .none }
                state.isLoading = false
                switch result {
                case .success(let text):
                    if let previous = state.previousTextForComparison, previous != text {
                        state.draft.comments = []
                        state.notice = "The document changed. Review it again before sending comments."
                    }
                    state.sourceText = text
                    state.previousTextForComparison = nil
                    state.loadError = nil
                case .failure(let error):
                    state.loadError = error.message
                }
                return .none
            case .generalChanged(let value):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.general = value
                return .none
            case .commentChanged(let anchor, let quote, let text):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
                if let index = state.draft.comments.firstIndex(where: { $0.anchor == anchor }) {
                    if trimmed.isEmpty { state.draft.comments.remove(at: index) }
                    else { state.draft.comments[index].text = trimmed }
                } else if !trimmed.isEmpty {
                    state.draft.comments.append(ReviewComment(anchor: anchor, quote: quote, text: trimmed))
                }
                return .none
            case .optionSelected(let option):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.selectedOption = option
                return .none
            case .attachmentsChanged(let ids):
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.attachmentIDs = ids
                return .none
            case .sendFeedbackTapped(let author):
                guard state.attachmentPicker.allReady else { return .none }
                guard canReview(state) else { return .none }
                do {
                    switch state.subject {
                    case .question(let question):
                        var answer = QuestionDraft()
                        answer.text = ReviewFeedback.format(state.draft.comments, general: state.draft.general, author: author)
                        answer.selectedOption = state.draft.selectedOption
                        answer.attachmentIDs = state.draft.attachmentIDs
                        return submit(try answerUseCase.answer(question, draft: answer), state: &state)
                    case .report(let report):
                        guard let identity = state.identity else {
                            state.notice = "Connect a paired device before sending feedback."
                            return .none
                        }
                        let request = try reviewUseCase.feedback(for: report, comments: state.draft.comments,
                                                                 general: state.draft.general, author: author,
                                                                 attachmentIDs: state.draft.attachmentIDs)
                        state.isSubmitting = true
                        state.notice = nil
                        state.conflict = nil
                        let revision = state.revision
                        return .run { send in
                            await send(.reportFeedbackFinished(revision, await service.submitReportFeedback(request, identity)))
                        }.cancellable(id: CancelID.submit)
                    }
                } catch {
                    state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription
                    return .none
                }
            case .reviewTapped(let verdict):
                guard state.attachmentPicker.allReady else { return .none }
                guard state.draft.attachmentIDs.isEmpty else {
                    state.notice = "Send feedback to include your attachments before approving the artifact."
                    return .none
                }
                guard canReview(state) else { return .none }
                guard case .report(let report) = state.subject else { return .none }
                do { return submit(try reviewUseCase.review(report, verdict: verdict), state: &state) }
                catch { state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription; return .none }
            case .feedbackReadRejected(let id, let message):
                guard state.needsReconciliation, state.uncertainCommandID == id else { return .none }
                state.notice = message
                state.feedbackReadRetryAvailable = true
                return .none
            case .retryFeedbackRead:
                guard let id = state.uncertainCommandID, state.needsReconciliation,
                      state.feedbackReadRetryAvailable, !state.isRetryingFeedbackRead else { return .none }
                state.isRetryingFeedbackRead = true
                return .run { send in
                    await service.retryReportRead(id)
                    await send(.feedbackReadRetryFinished(id))
                }.cancellable(id: CancelID.retryRead, cancelInFlight: true)
            case .feedbackReadRetryFinished(let id):
                guard state.uncertainCommandID == id else { return .none }
                state.isRetryingFeedbackRead = false
                return .none
            case .commandResolved(let id, let rejection):
                guard state.needsReconciliation, state.uncertainCommandID == id else { return .none }
                state.feedbackReadRetryAvailable = false
                state.isRetryingFeedbackRead = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.notice = rejection
                if rejection == nil {
                    state.completed = true
                    state.draft = DocumentDraft()
                    state.attachmentPicker = AttachmentFeature.State()
                }
                return .none
            case .reconciled(let cardStillOpen):
                guard state.needsReconciliation else { return .none }
                state.feedbackReadRetryAvailable = false
                state.isRetryingFeedbackRead = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.completed = !cardStillOpen
                state.notice = nil
                if !cardStillOpen { state.draft = DocumentDraft() }
                return .none
            case .submissionFinished(let revision, _, let outcome), .reportFeedbackFinished(let revision, let outcome):
                guard state.revision == revision, state.isSubmitting else { return .none }
                state.isSubmitting = false
                switch outcome {
                case .confirmed:
                    state.completed = true
                    state.draft = DocumentDraft()
                    state.attachmentPicker = AttachmentFeature.State()
                case .conflict(let reason): state.conflict = reason
                case .forbidden(let reason), .failed(let reason): state.notice = reason
                case .uncertainCommand(let id):
                    state.uncertainCommandID = id
                    state.notice = "The result is uncertain. Checking its status…"
                    state.needsReconciliation = true
                case .uncertain:
                    state.notice = "The result is uncertain. Check the server before trying again."
                    state.needsReconciliation = true
                case .obsolete: break
                }
                return .none
            case .dismissNotice:
                state.notice = nil
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in
                AttachmentFeature.cancelEffects(instanceID: oldID).map(Action.attachmentPicker)
            }
        }
    }

    private func load(_ state: inout State) -> Effect<Action> {
        state.isLoading = true
        // A failed reload has no sourceText, but its last successful body remains the
        // comparison baseline for the next retry. Otherwise V2 could retain V1 anchors.
        state.previousTextForComparison = state.sourceText ?? state.previousTextForComparison
        state.sourceText = nil
        state.loadError = nil
        state.loadRevision &+= 1
        let revision = state.loadRevision
        let sources = state.subject.sources
        return .run { send in
            do {
                var documents: [String] = []
                for source in sources {
                    try Task.checkCancellation()
                    switch source {
                    case .inline(let inline): documents.append(inline)
                    case .stored(let name, let path):
                        documents.append("# \(name)\n\n" + (try await service.document(path)))
                    }
                }
                await send(.loaded(revision, .success(documents.joined(separator: "\n\n---\n\n"))))
            } catch is CancellationError {
            } catch {
                await send(.loaded(revision, .failure(DocumentLoadFailure(message: error.localizedDescription))))
            }
        }.cancellable(id: CancelID.load, cancelInFlight: true)
    }

    private func canReview(_ state: State) -> Bool {
        !state.isLoading && state.loadError == nil && state.sourceText != nil &&
            !state.isSubmitting && !state.completed && !state.needsReconciliation
    }

    private func submit(_ operation: QuestionOperation, state: inout State) -> Effect<Action> {
        state.isSubmitting = true
        state.notice = nil
        state.conflict = nil
        let revision = state.revision
        return .run { send in
            await send(.submissionFinished(revision, operation, await service.submit(operation)))
        }.cancellable(id: CancelID.submit)
    }
}

public struct DocumentLoadFailure: Error, Equatable, Sendable {
    public let message: String
    public init(message: String) { self.message = message }
}
