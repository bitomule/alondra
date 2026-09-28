import AlondraContracts
import AlondraDomain
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
            let attachments = report.attachments.filter { report.kind == "document" && $0.isText }.compactMap { attachment in
                attachment.storedPath.map { DocumentSource.stored(name: attachment.name, path: $0) }
            }
            let body = report.body ?? ""
            return body.isEmpty && !attachments.isEmpty ? attachments : [.inline(body)] + attachments
        }
    }
}

public enum DocumentSourcePolicy: Equatable, Sendable {
    case allSections
    case phone
}

extension DocumentSubject {
    public func sources(for policy: DocumentSourcePolicy) -> [DocumentSource] {
        guard policy == .phone else { return sources }
        switch self {
        case .question(let question):
            if question.origin == "plan", let detail = question.detail { return [.inline(detail)] }
            if let attachment = question.attachments.first(where: { $0.storedPath != nil }), let path = attachment.storedPath {
                return [.stored(name: attachment.name, path: path)]
            }
            return [.inline(question.readableDetail ?? "")]
        case .report(let report):
            if let attachment = report.attachments.first(where: { $0.storedPath != nil }), let path = attachment.storedPath {
                return [.stored(name: attachment.name, path: path)]
            }
            return [.inline(report.body ?? "")]
        }
    }
}

public enum DocumentSource: Equatable, Sendable {
    case inline(String)
    case stored(name: String, path: String)
}

public struct DocumentSection: Equatable, Sendable, Identifiable {
    public let source: DocumentSource
    public var text: String
    public var isLoading: Bool
    public var error: String?
    public var id: String {
        switch source { case .inline: "body"; case .stored(_, let path): path }
    }
    public init(source: DocumentSource, text: String, isLoading: Bool = false, error: String? = nil) {
        self.source = source; self.text = text; self.isLoading = isLoading; self.error = error
    }
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
    public enum PendingReview: Equatable, Sendable { case feedback(String), moreContext(String) }
    @ObservableState
    public struct State: Equatable, Identifiable {
        public var id: String { subject.id }
        public var downloads = AttachmentDownloadFeature.State()
        public var attachmentPicker = AttachmentFeature.State()
        public var subject: DocumentSubject
        public var sourcePolicy: DocumentSourcePolicy = .allSections
        public var explanationPolicy: QuestionExplanationPolicy { sourcePolicy == .phone ? .phone : .desktop }
        public var identity: QuestionWorkIdentity?
        public var sourceText: String?
        public var sections: [DocumentSection] = []
        public var previousTextForComparison: String?
        public var isLoading = false
        public var loadError: String?
        public var feedbackAuthor = ""
        public var draft = DocumentDraft()
        public var composingAnchor: String?
        public var composingText = ""
        public var commentPositions: [String: String] = [:]
        public var generalFocusRequest: UInt64 = 0
        /// Feedback anchors on the loaded document, so review actions wait for it.
        public var canReview: Bool {
            !isLoading && loadError == nil && sourceText != nil &&
                !isSubmitting && pendingSubmission == nil && !completed && !needsReconciliation
        }
        public var hasFeedback: Bool {
            !draft.comments.isEmpty || !draft.general.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachmentPicker.entries.isEmpty
        }
        public var approveOption: String? {
            guard case .question(let question) = subject else { return nil }
            return DocumentReviewChoices().approveOption(question)
        }
        public var changeOption: String? {
            guard case .question(let question) = subject else { return nil }
            return DocumentReviewChoices().changeOption(question)
        }
        public var isSubmitting = false
        public var pendingSubmission: AttachmentSubmissionIntent?
        public var pendingReview: PendingReview?
        public var keyboardReady = false
        public var completed = false
        public var uncertainCommandID: UUID?
        public var feedbackReadRetryAvailable = false
        public var isRetryingFeedbackRead = false
        public var needsReconciliation = false
        public var deliveryUnconfirmed = false
        public var deliveryDiscarded = false
        public var deliveryAttemptID: UInt64?
        public var conflict: String?
        public var notice: String?
        /// Only terminal command failures use the card toast; local/recovery warnings stay inline.
        public var noticeIsSubmissionFailure = false
        public var revision: UInt64 = 0
        public var loadRevision: UInt64 = 0

        /// Drops only an upload-waiting gesture; drafts, receipts and dispatched commands remain intact.
        public mutating func cancelPendingSubmission() {
            pendingSubmission = nil
            pendingReview = nil
        }

        public init(subject: DocumentSubject, identity: QuestionWorkIdentity? = nil, feedbackAuthor: String = "") {
            self.feedbackAuthor = feedbackAuthor
            self.subject = subject
            self.identity = identity
        }
    }

    public enum Action {
        case keyboardAppeared, keyboardDisappeared, keyboardReady
        case downloads(AttachmentDownloadFeature.Action)
        case attachmentPicker(AttachmentFeature.Action)
        case appeared
        case phoneAppeared
        case disappeared
        case refreshed(DocumentSubject)
        case identityChanged(QuestionWorkIdentity?)
        case loaded(UInt64, Result<String, DocumentLoadFailure>, sections: [DocumentSection] = [])
        case sectionLoaded(UInt64, source: DocumentSource, Result<String, DocumentLoadFailure>)
        case chooseReviewOption(QuestionOption, author: String)
        case submitFeedback(author: String)
        case approveQuestion
        case moreContextTapped(author: String)
        case composeComment(String?)
        case composingTextChanged(String)
        case saveComposedComment(anchor: String, quote: String, position: String)
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
        case commandDeliveryUnconfirmed(UUID, attemptID: UInt64?)
        case deliveryDecisionResolved(retry: Bool, discarded: Bool = false)
        case reconciled(cardStillOpen: Bool)
        case submissionFinished(UInt64, QuestionOperation, QuestionWorkOutcome)
        case reportFeedbackFinished(UInt64, QuestionWorkOutcome)
        case dismissNotice
    }

    @Dependency(\.continuousClock) var clock
    @Dependency(\.questionWorkService) var service
    private let answerUseCase = AnswerQuestionUseCase()
    private let reviewUseCase = ReviewDocumentUseCase()
    private enum CancelID { case keyboard, load, submit, retryRead }

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
                if action.invalidatesPendingSubmission { clearPending(&state); return .none }
                guard state.pendingSubmission != nil else { return .none }
                if case .uploaded(_, .failure(let error)) = action {
                    clearPending(&state)
                    state.noticeIsSubmissionFailure = true
                    state.notice = error.message
                    return .none
                }
                guard state.attachmentPicker.allReady, let review = state.pendingReview else { return .none }
                clearPending(&state)
                return submitReady(review, state: &state)
            case .phoneAppeared:
                state.sourcePolicy = .phone
                return load(&state)
            case .appeared:
                return load(&state)
            case .disappeared:
                clearPending(&state)
                return .cancel(id: CancelID.load)
            case .refreshed(let subject):
                guard state.subject.id == subject.id else {
                    let revision = state.revision &+ 1
                    let loadRevision = state.loadRevision &+ 1
                    let sourcePolicy = state.sourcePolicy
                    state = State(subject: subject, identity: state.identity)
                    state.sourcePolicy = sourcePolicy
                    state.revision = revision
                    state.loadRevision = loadRevision
                    // load() replaces the prior load through cancelInFlight. Cancel submission
                    // first so an old card's acknowledgement cannot race this card's new load.
                    return .concatenate(.cancel(id: CancelID.submit), load(&state))
                }
                if state.subject != subject { clearPending(&state) }
                let oldSource = state.subject.sources(for: state.sourcePolicy)
                state.subject = subject
                if oldSource != subject.sources(for: state.sourcePolicy) {
                    state.draft.comments = []
                    state.noticeIsSubmissionFailure = false
                    state.notice = "The document changed. Review it again before sending comments."
                    return load(&state)
                }
                return .none
            case .identityChanged(let identity):
                clearPending(&state)
                state.attachmentPicker = AttachmentFeature.State()
                state.identity = identity
                state.feedbackAuthor = ""
                state.draft = DocumentDraft()
                state.sourceText = nil
                state.sections = []
                state.previousTextForComparison = nil
                state.isLoading = false
                state.loadError = nil
                state.isSubmitting = false
                state.completed = false
                state.feedbackReadRetryAvailable = false
                state.isRetryingFeedbackRead = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.deliveryUnconfirmed = false; state.deliveryDiscarded = false; state.deliveryAttemptID = nil
                state.conflict = nil
                state.noticeIsSubmissionFailure = false
                state.notice = nil
                state.revision &+= 1
                state.loadRevision &+= 1
                return .merge(.cancel(id: CancelID.load), .cancel(id: CancelID.submit), .cancel(id: CancelID.retryRead))
            case .sectionLoaded(let revision, let source, let result):
                guard state.loadRevision == revision, state.isLoading,
                      let index = state.sections.firstIndex(where: { $0.source == source }) else { return .none }
                state.sections[index].isLoading = false
                switch result {
                case .success(let text): state.sections[index].text = text
                case .failure(let error): state.sections[index].error = error.message
                }
                return .none
            case .loaded(let revision, let result, let sections):
                guard state.loadRevision == revision else { return .none }
                state.isLoading = false
                state.sections = sections
                switch result {
                case .success(let text):
                    if let previous = state.previousTextForComparison, previous != text {
                        state.draft.comments = []
                        state.noticeIsSubmissionFailure = false
                        state.notice = "The document changed. Review it again before sending comments."
                    }
                    state.sourceText = text
                    state.sections = sections
                    state.previousTextForComparison = nil
                    state.loadError = nil
                case .failure(let error):
                    state.loadError = error.message
                }
                return .none
            case .chooseReviewOption(let option, let author):
                guard state.canReview else { return .none }
                state.draft.selectedOption = option.label
                let hasExplanation = state.explanationPolicy == .phone ? state.hasFeedback
                    : !ReviewFeedback.format(state.draft.comments, general: state.draft.general, author: author).isEmpty
                if DocumentReviewChoices().requiresText(option, policy: state.explanationPolicy), !hasExplanation {
                    state.generalFocusRequest &+= 1
                    return .none
                }
                state.draft.selectedOption = option.label
                return .send(.sendFeedbackTapped(author: author))
            case .submitFeedback(let author):
                guard state.canReview, state.hasFeedback else { return .none }
                state.draft.selectedOption = state.draft.selectedOption ?? state.changeOption
                return .send(.sendFeedbackTapped(author: author))
            case .moreContextTapped(let author):
                return prepare(.moreContext(author), state: &state)
            case .approveQuestion:
                guard state.canReview, case .question(let question) = state.subject, let option = state.approveOption else { return .none }
                var draft = QuestionDraft()
                draft.selectedOption = option
                do { return submit(try answerUseCase.answer(question, draft: draft, explanationPolicy: state.explanationPolicy), state: &state) }
                catch { state.noticeIsSubmissionFailure = false; state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription; return .none }
            case .composeComment(let anchor):
                clearPending(&state)
                state.composingAnchor = anchor
                state.composingText = state.draft.comments.first { $0.anchor == anchor }?.text ?? ""
                return .none
            case .saveComposedComment(let anchor, let quote, let position):
                clearPending(&state)
                guard !state.isSubmitting, !state.needsReconciliation, state.composingAnchor == anchor else { return .none }
                state.commentPositions[anchor] = position
                state.draft.comments = DocumentCommentUseCase().updating(state.draft.comments, anchor: anchor, quote: quote, text: state.composingText, positions: state.commentPositions)
                state.composingAnchor = nil
                state.composingText = ""
                return .none
            case .composingTextChanged(let value):
                clearPending(&state)
                state.composingText = value
                return .none
            case .generalChanged(let value):
                clearPending(&state)
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.general = value
                return .none
            case .commentChanged(let anchor, let quote, let text):
                clearPending(&state)
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.comments = DocumentCommentUseCase().updating(state.draft.comments, anchor: anchor, quote: quote, text: text, positions: state.commentPositions)
                return .none
            case .optionSelected(let option):
                clearPending(&state)
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.selectedOption = option
                return .none
            case .attachmentsChanged(let ids):
                clearPending(&state)
                guard !state.isSubmitting, !state.needsReconciliation else { return .none }
                state.draft.attachmentIDs = ids
                return .none
            case .sendFeedbackTapped(let author):
                return prepare(.feedback(author), state: &state)
            case .reviewTapped(let verdict):
                guard state.attachmentPicker.allReady else { return .none }
                guard state.draft.attachmentIDs.isEmpty else {
                    state.noticeIsSubmissionFailure = false
                    state.notice = "Send feedback to include your attachments before approving the artifact."
                    return .none
                }
                guard state.canReview else { return .none }
                guard case .report(let report) = state.subject else { return .none }
                do { return submit(try reviewUseCase.review(report, verdict: verdict), state: &state) }
                catch { state.noticeIsSubmissionFailure = false; state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription; return .none }
            case .feedbackReadRejected(let id, let message):
                guard state.needsReconciliation, state.uncertainCommandID == id else { return .none }
                state.noticeIsSubmissionFailure = false
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
            case .commandDeliveryUnconfirmed(let id, let attemptID):
                guard state.needsReconciliation, state.uncertainCommandID == id else { return .none }
                state.uncertainCommandID = nil
                state.deliveryUnconfirmed = true; state.deliveryAttemptID = attemptID
                state.noticeIsSubmissionFailure = false; state.notice = DeliveryHandoffUnconfirmed.message + " Your draft has been kept."
                return .none
            case .deliveryDecisionResolved(let retry, let discarded):
                guard state.deliveryUnconfirmed else { return .none }
                state.deliveryDiscarded = discarded
                state.deliveryUnconfirmed = false; state.deliveryAttemptID = nil; state.needsReconciliation = false
                state.notice = nil; state.noticeIsSubmissionFailure = false; state.completed = !retry
                return .none
            case .commandResolved(let id, let rejection):
                guard state.needsReconciliation, state.uncertainCommandID == id else { return .none }
                state.feedbackReadRetryAvailable = false
                state.isRetryingFeedbackRead = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.noticeIsSubmissionFailure = rejection != nil
                state.notice = rejection
                if rejection == nil {
                    state.completed = true
                    state.draft = DocumentDraft()
                    state.attachmentPicker = AttachmentFeature.State()
                }
                return .none
            case .reconciled(let cardStillOpen):
                guard state.needsReconciliation, !state.deliveryUnconfirmed else { return .none }
                state.feedbackReadRetryAvailable = false
                state.isRetryingFeedbackRead = false
                state.uncertainCommandID = nil
                state.needsReconciliation = false
                state.completed = !cardStillOpen
                state.noticeIsSubmissionFailure = false
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
                case .conflict(let reason): state.conflict = reason; state.noticeIsSubmissionFailure = true
                case .forbidden(let reason), .failed(let reason): state.notice = reason; state.noticeIsSubmissionFailure = true
                case .uncertainCommand(let id):
                    state.uncertainCommandID = id
                    state.noticeIsSubmissionFailure = false
                    state.notice = "The result is uncertain. Checking its status…"
                    state.needsReconciliation = true
                case .uncertain:
                    state.noticeIsSubmissionFailure = false
                    state.notice = "We couldn’t confirm the result. Your draft has been kept."
                    state.needsReconciliation = true
                case .deliveryUnconfirmed(let attemptID):
                    state.deliveryUnconfirmed = true; state.deliveryAttemptID = attemptID; state.needsReconciliation = true
                    state.noticeIsSubmissionFailure = false; state.notice = DeliveryHandoffUnconfirmed.message + " Your draft has been kept."
                case .obsolete: break
                }
                return .none
            case .dismissNotice:
                state.noticeIsSubmissionFailure = false
                state.notice = nil
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in
                AttachmentFeature.cancelEffects(instanceID: oldID).map { .attachmentPicker($0) }
            }
        }
    }

    private func load(_ state: inout State) -> Effect<Action> {
        clearPending(&state)
        state.isLoading = true
        // A failed reload has no sourceText, but its last successful body remains the
        // comparison baseline for the next retry. Otherwise V2 could retain V1 anchors.
        state.previousTextForComparison = state.sourceText ?? state.previousTextForComparison
        state.sourceText = nil
        state.sections = state.subject.sources(for: state.sourcePolicy).map { source in
            switch source {
            case .inline(let text): DocumentSection(source: source, text: text)
            case .stored: DocumentSection(source: source, text: "", isLoading: true)
            }
        }
        state.loadError = nil
        state.loadRevision &+= 1
        let revision = state.loadRevision
        let sources = state.subject.sources(for: state.sourcePolicy)
        let sourcePolicy = state.sourcePolicy
        return .run { send in
            do {
                var documents: [String] = []
                var sections: [DocumentSection] = []
                var firstError: DocumentLoadFailure?
                for source in sources {
                    try Task.checkCancellation()
                    switch source {
                    case .inline(let inline):
                        documents.append(inline)
                        sections.append(DocumentSection(source: source, text: inline))
                    case .stored(let name, let path):
                        do {
                            let text = try await service.document(path)
                            try Task.checkCancellation()
                            documents.append(sourcePolicy == .phone ? text : "# \(name)\n\n" + text)
                            sections.append(DocumentSection(source: source, text: text))
                            await send(.sectionLoaded(revision, source: source, .success(text)))
                        } catch is CancellationError { throw CancellationError() }
                        catch {
                            let failure = DocumentLoadFailure(message: error.localizedDescription)
                            firstError = firstError ?? failure
                            sections.append(DocumentSection(source: source, text: "", error: failure.message))
                            await send(.sectionLoaded(revision, source: source, .failure(failure)))
                        }
                    }
                }
                if let firstError { await send(.loaded(revision, .failure(firstError), sections: sections)) }
                else { await send(.loaded(revision, .success(documents.joined(separator: "\n\n---\n\n")), sections: sections)) }
            } catch is CancellationError {
            } catch {
                await send(.loaded(revision, .failure(DocumentLoadFailure(message: error.localizedDescription))))
            }
        }.cancellable(id: CancelID.load, cancelInFlight: true)
    }

    private func clearPending(_ state: inout State) { state.cancelPendingSubmission() }

    private func prepare(_ review: PendingReview, state: inout State) -> Effect<Action> {
        guard state.canReview, state.attachmentPicker.canPrepareSubmission else { return .none }
        if !state.attachmentPicker.allReady {
            state.pendingSubmission = .awaitingReceipts
            state.pendingReview = review
            return .send(.attachmentPicker(.prepareForSubmission))
        }
        return submitReady(review, state: &state)
    }

    private func submitReady(_ review: PendingReview, state: inout State) -> Effect<Action> {
        guard state.canReview, state.attachmentPicker.allReady else { return .none }
        switch review {
        case .moreContext(let author):
            guard case .question(let question) = state.subject else { return .none }
            var draft = QuestionDraft()
            draft.text = ReviewFeedback.format(state.draft.comments, general: state.draft.general, author: author)
            draft.attachmentIDs = state.attachmentPicker.storedIDs
            do { return submit(try answerUseCase.checkedMoreContext(question, draft: draft), state: &state) }
            catch { state.noticeIsSubmissionFailure = false; state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription; return .none }
        case .feedback(let author):
            do {
                switch state.subject {
                case .question(let question):
                    var answer = QuestionDraft()
                    answer.text = ReviewFeedback.format(state.draft.comments, general: state.draft.general, author: author)
                    answer.selectedOption = state.draft.selectedOption
                    answer.attachmentIDs = state.draft.attachmentIDs
                    return submit(try answerUseCase.answer(question, draft: answer, explanationPolicy: state.explanationPolicy), state: &state)
                case .report(let report):
                    guard let identity = state.identity else {
                        state.noticeIsSubmissionFailure = false
                        state.notice = "Connect a paired device before sending feedback."
                        return .none
                    }
                    let request = try reviewUseCase.feedback(for: report, comments: state.draft.comments,
                                                             general: state.draft.general, author: author,
                                                             attachmentIDs: state.draft.attachmentIDs)
                    state.isSubmitting = true
                    state.noticeIsSubmissionFailure = false
                    state.notice = nil
                    state.conflict = nil
                    let revision = state.revision
                    return .run { send in
                        await send(.reportFeedbackFinished(revision, await service.submitReportFeedback(request, identity)))
                    }.cancellable(id: CancelID.submit)
                }
            } catch {
                state.noticeIsSubmissionFailure = false
                state.notice = (error as? QuestionWorkError)?.message ?? error.localizedDescription
                return .none
            }
        }
    }

    private func submit(_ operation: QuestionOperation, state: inout State) -> Effect<Action> {
        state.isSubmitting = true
        state.noticeIsSubmissionFailure = false
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
