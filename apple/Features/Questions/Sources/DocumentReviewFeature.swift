import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

/// What the server presented, read as a document: information, a delivery with its exact
/// targets, or a question whose body is long enough to review. Document content is the
/// immutable DocumentRevision a delivery names, never a URL.
public struct DocumentSubject: Equatable, Sendable {
    public let item: DomainV2.InboxItem
    public let interaction: DomainV2.Interaction
    public let revisions: [RevisionSource]

    public struct RevisionSource: Equatable, Sendable {
        public let title: String
        public let revision: DomainV2.DocumentRevision
    }

    public init?(item: DomainV2.InboxItem, in workspace: DomainWorkspace) {
        guard let interaction = workspace.interaction(of: item) else { return nil }
        switch interaction.payload {
        case .information, .question, .actionRequest: revisions = []
        case .delivery(let targets, _, _, _):
            revisions = targets.compactMap { target in
                guard case let .documentRevision(documentID, revisionID) = target,
                      let revision = workspace.documentRevision(revisionID) else { return nil }
                return RevisionSource(title: workspace.document(documentID)?.title ?? documentID.rawValue, revision: revision)
            }
        }
        self.item = item
        self.interaction = interaction
    }

    public var id: String { item.id }
    public var title: String { item.title }
    public var isDelivery: Bool {
        if case .delivery = interaction.payload { return true }
        return false
    }
    public var isInformation: Bool {
        if case .information = interaction.payload { return true }
        return false
    }
    public var question: DomainQuestion? {
        switch interaction.payload {
        case .question, .actionRequest: DomainQuestion(item: item, interaction: interaction)
        default: nil
        }
    }
    public var attachments: [DomainV2.AttachmentRef] { interaction.attachments }

    public func action(_ kind: DomainV2.ActionKind) -> DomainV2.AvailableAction? {
        item.availableActions.first { $0.actionKind == kind }
    }

    public var sources: [DocumentSource] {
        var sources: [DocumentSource] = interaction.body.isEmpty ? [] : [.inline(interaction.body)]
        if case .delivery(let targets, _, _, _) = interaction.payload {
            for case .text(let text) in targets where !text.isEmpty { sources.append(.inline(text)) }
        }
        let texts = attachments.filter(\.isText).map(DocumentSource.attachment)
        return sources + revisions.map { .revision(name: $0.title, id: $0.revision.id, digest: $0.revision.contentDigest) } + texts
    }
}

public enum DocumentSource: Equatable, Sendable {
    case inline(String)
    case revision(name: String, id: DomainV2.DocumentRevisionId, digest: String)
    case attachment(DomainV2.AttachmentRef)
}

extension DomainV2.AttachmentRef {
    /// Text attachments render inline as part of the document.
    public var isText: Bool {
        ["md", "markdown", "txt", "text", ""].contains((name as NSString).pathExtension.lowercased())
    }
}

public struct DocumentSection: Equatable, Sendable, Identifiable {
    public let source: DocumentSource
    public var text: String
    public var isLoading: Bool
    public var error: String?
    public var id: String {
        switch source {
        case .inline(let text): "inline:\(text.hashValue)"
        case .revision(_, let id, _): id.rawValue
        case .attachment(let attachment): "attachment:" + attachment.id
        }
    }
    public init(source: DocumentSource, text: String, isLoading: Bool = false, error: String? = nil) {
        self.source = source; self.text = text; self.isLoading = isLoading; self.error = error
    }
}

public struct DocumentDraft: Equatable, Sendable {
    public var comments: [ReviewComment] = []
    public var general = ""
    public init() {}
}

/// Reads and reviews one presented item as a document:
/// - a delivery is accepted with `accept_delivery` or answered with one `submit_feedback`;
/// - information is replied to with `reply_to_information`; reading alone never replies;
/// - a question is answered with one of its server options, or its Agent is asked for context.
@Reducer
public struct DocumentReviewFeature {
    public enum PendingReview: Equatable, Sendable {
        case feedback(String), accept, reply(String), answer(option: String, text: String), moreContext(String)
    }

    @ObservableState
    public struct State: Equatable, Identifiable {
        public var id: String { subject.id }
        public var downloads = AttachmentDownloadFeature.State()
        public var attachmentPicker = AttachmentFeature.State()
        public var subject: DocumentSubject
        public var workspace: DomainWorkspace
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
        public var isSubmitting = false
        /// Waiting for attachment uploads before sending; never a retry of a sent command.
        public var pendingReview: PendingReview?
        public var keyboardReady = false
        public var completed = false
        /// The command ID of an attempt whose outcome is unknown; sending again reuses it.
        public var commandID: String?
        public var commandReview: PendingReview?
        public var needsReconciliation = false
        public var conflict: String?
        public var notice: String?
        /// Only terminal command failures use the card toast; local/recovery warnings stay inline.
        public var noticeIsSubmissionFailure = false
        public var revision: UInt64 = 0
        public var loadRevision: UInt64 = 0

        /// Feedback anchors on the loaded document, so review actions wait for it.
        public var canReview: Bool {
            !isLoading && loadError == nil && sourceText != nil && !isSubmitting && pendingReview == nil
                && !completed && attachmentPicker.canPrepareSubmission
        }
        public var canAccept: Bool { canReview && subject.action(.acceptDelivery)?.enabled == true }
        public var canSendFeedback: Bool { canReview && subject.action(.submitFeedback)?.enabled == true }
        public var canReply: Bool { canReview && subject.isInformation }
        public var canAnswer: Bool { canReview && subject.question?.canAnswer == true }
        public var hasFeedback: Bool {
            !draft.comments.isEmpty || !draft.general.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                || !attachmentPicker.entries.isEmpty
        }

        /// Drops only an upload-waiting gesture; drafts, receipts and dispatched commands remain intact.
        public mutating func cancelPendingSubmission() { pendingReview = nil }

        public init(subject: DocumentSubject, workspace: DomainWorkspace, identity: QuestionWorkIdentity? = nil,
                    feedbackAuthor: String = "") {
            self.feedbackAuthor = feedbackAuthor
            self.subject = subject
            self.workspace = workspace
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
        case refreshed(DocumentSubject, DomainWorkspace)
        case identityChanged(QuestionWorkIdentity?)
        case loaded(UInt64, Result<String, DocumentLoadFailure>, sections: [DocumentSection] = [])
        case sectionLoaded(UInt64, source: DocumentSource, Result<String, DocumentLoadFailure>)
        case composeComment(String?)
        case composingTextChanged(String)
        case saveComposedComment(anchor: String, quote: String, position: String)
        case generalChanged(String)
        case commentChanged(anchor: String, quote: String, text: String)
        case sendFeedbackTapped(author: String)
        case reviewTapped(ReviewVerdict)
        case chooseReviewOption(String, author: String)
        case moreContextTapped(author: String)
        case submissionFinished(UInt64, DomainCommandResult)
        case dismissNotice
    }

    @Dependency(\.continuousClock) var clock
    @Dependency(\.domainWorkspace) var client
    @Dependency(\.uuid) var uuid
    @Dependency(\.attachmentDownload) var attachments
    private enum CancelID { case keyboard, load, submit }

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
                    state.commandID = nil
                    state.needsReconciliation = false
                    return .none
                }
                guard let review = state.pendingReview else { return .none }
                if case .uploaded(_, .failure(let error)) = action {
                    state.cancelPendingSubmission()
                    state.noticeIsSubmissionFailure = true
                    state.notice = error.message
                    return .none
                }
                guard state.attachmentPicker.allReady else { return .none }
                state.cancelPendingSubmission()
                return submit(review, &state)
            case .appeared, .phoneAppeared:
                return load(&state)
            case .disappeared:
                state.cancelPendingSubmission()
                return .cancel(id: CancelID.load)
            case let .refreshed(subject, workspace):
                guard state.subject.id == subject.id else {
                    let revision = state.revision &+ 1
                    let loadRevision = state.loadRevision &+ 1
                    state = State(subject: subject, workspace: workspace, identity: state.identity,
                                  feedbackAuthor: state.feedbackAuthor)
                    state.revision = revision
                    state.loadRevision = loadRevision
                    return .concatenate(.cancel(id: CancelID.submit), load(&state))
                }
                if state.subject != subject { state.cancelPendingSubmission() }
                let oldSources = state.subject.sources
                state.subject = subject
                state.workspace = workspace
                if oldSources != subject.sources {
                    state.draft.comments = []
                    state.noticeIsSubmissionFailure = false
                    state.notice = String(localized: "The document changed. Review it again before sending comments.")
                    return load(&state)
                }
                return .none
            case .identityChanged(let identity):
                state.cancelPendingSubmission()
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
                state.commandID = nil
                state.needsReconciliation = false
                state.conflict = nil
                state.noticeIsSubmissionFailure = false
                state.notice = nil
                state.revision &+= 1
                state.loadRevision &+= 1
                return .merge(.cancel(id: CancelID.load), .cancel(id: CancelID.submit))
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
                        state.notice = String(localized: "The document changed. Review it again before sending comments.")
                    }
                    state.sourceText = text
                    state.previousTextForComparison = nil
                    state.loadError = nil
                case .failure(let error):
                    state.loadError = error.message
                }
                return .none
            case .composeComment(let anchor):
                state.cancelPendingSubmission()
                state.composingAnchor = anchor
                state.composingText = state.draft.comments.first { $0.anchor == anchor }?.text ?? ""
                return .none
            case .saveComposedComment(let anchor, let quote, let position):
                state.cancelPendingSubmission()
                guard !state.isSubmitting, state.composingAnchor == anchor else { return .none }
                state.commandID = nil
                state.needsReconciliation = false
                state.commentPositions[anchor] = position
                state.draft.comments = DocumentCommentUseCase().updating(state.draft.comments, anchor: anchor, quote: quote, text: state.composingText, positions: state.commentPositions)
                state.composingAnchor = nil
                state.composingText = ""
                return .none
            case .composingTextChanged(let value):
                state.cancelPendingSubmission()
                state.composingText = value
                return .none
            case .generalChanged(let value):
                state.cancelPendingSubmission()
                guard !state.isSubmitting else { return .none }
                if state.draft.general != value { state.commandID = nil; state.needsReconciliation = false }
                state.draft.general = value
                return .none
            case .commentChanged(let anchor, let quote, let text):
                state.cancelPendingSubmission()
                guard !state.isSubmitting else { return .none }
                state.commandID = nil
                state.needsReconciliation = false
                state.draft.comments = DocumentCommentUseCase().updating(state.draft.comments, anchor: anchor, quote: quote, text: text, positions: state.commentPositions)
                return .none
            case .sendFeedbackTapped(let author):
                let body = ReviewFeedback.format(state.draft.comments, general: state.draft.general, author: author)
                if state.subject.isInformation {
                    guard state.canReply else { return .none }
                    guard !body.isEmpty || !state.attachmentPicker.entries.isEmpty else {
                        state.generalFocusRequest &+= 1
                        return .none
                    }
                    return prepare(.reply(body), &state)
                }
                guard state.canSendFeedback else { return .none }
                guard !body.isEmpty else {
                    state.generalFocusRequest &+= 1
                    return .none
                }
                return prepare(.feedback(body), &state)
            case .reviewTapped(let verdict):
                switch verdict {
                case .approved:
                    guard state.canAccept else { return .none }
                    guard !state.hasFeedback else {
                        state.noticeIsSubmissionFailure = false
                        state.notice = String(localized: "Send your comments or clear them before accepting.")
                        return .none
                    }
                    return prepare(.accept, &state)
                case .comments:
                    return .send(.sendFeedbackTapped(author: state.feedbackAuthor))
                }
            case let .chooseReviewOption(optionID, author):
                guard state.canAnswer, let option = state.subject.question?.option(optionID) else { return .none }
                let text = ReviewFeedback.format(state.draft.comments, general: state.draft.general, author: author)
                if AnswerForm(option: option).requiresText, text.isEmpty {
                    state.generalFocusRequest &+= 1
                    return .none
                }
                return prepare(.answer(option: option.id, text: text), &state)
            case .moreContextTapped(let author):
                guard state.canReview, state.subject.question != nil else { return .none }
                return prepare(.moreContext(ReviewFeedback.format(state.draft.comments, general: state.draft.general, author: author)), &state)
            case .submissionFinished(let revision, let result):
                guard state.revision == revision, state.isSubmitting else { return .none }
                state.isSubmitting = false
                if result != .uncertain { state.commandID = nil; state.needsReconciliation = false }
                switch result {
                case .recorded:
                    state.completed = true
                    state.draft = DocumentDraft()
                    state.attachmentPicker = AttachmentFeature.State()
                case .rejected(let rejection):
                    if rejection.errorCode == .staleRevision || rejection.errorCode == .acceptanceStale {
                        state.conflict = rejection.localizedMessage
                    } else {
                        state.notice = rejection.localizedMessage
                    }
                    state.noticeIsSubmissionFailure = true
                case .uncertain:
                    state.noticeIsSubmissionFailure = false
                    state.notice = String(localized: "We couldn’t confirm the result. Your draft is kept; sending it again won’t apply it twice.")
                    state.needsReconciliation = true
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
        state.isLoading = true
        // A failed reload has no sourceText, but its last successful body remains the
        // comparison baseline for the next retry, so old anchors are never kept.
        state.previousTextForComparison = state.sourceText ?? state.previousTextForComparison
        state.sourceText = nil
        let sources = state.subject.sources
        state.sections = sources.map { source in
            switch source {
            case .inline(let text): DocumentSection(source: source, text: text)
            case .revision, .attachment: DocumentSection(source: source, text: "", isLoading: true)
            }
        }
        state.loadError = nil
        state.loadRevision &+= 1
        let revision = state.loadRevision
        return .run { send in
            do {
                var documents: [String] = []
                var sections: [DocumentSection] = []
                var firstError: DocumentLoadFailure?
                for source in sources {
                    try Task.checkCancellation()
                    let named: (String, () async throws -> String)?
                    switch source {
                    case .inline(let inline):
                        documents.append(inline)
                        sections.append(DocumentSection(source: source, text: inline))
                        named = nil
                    case let .revision(name, id, digest):
                        named = (name, { try await client.documentContent(id, digest) })
                    case .attachment(let attachment):
                        named = (attachment.name, {
                            let file = try await attachments.download(attachment, UUID())
                            defer { Task { await attachments.release(file) } }
                            return String(decoding: try Data(contentsOf: file.url), as: UTF8.self)
                        })
                    }
                    guard let (name, read) = named else { continue }
                    do {
                        let text = try await read()
                        try Task.checkCancellation()
                        documents.append("# \(name)\n\n" + text)
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
                if let firstError { await send(.loaded(revision, .failure(firstError), sections: sections)) }
                else { await send(.loaded(revision, .success(documents.joined(separator: "\n\n---\n\n")), sections: sections)) }
            } catch is CancellationError {
            } catch {
                await send(.loaded(revision, .failure(DocumentLoadFailure(message: error.localizedDescription))))
            }
        }.cancellable(id: CancelID.load, cancelInFlight: true)
    }

    /// Uploads finish before the command is built, so the command carries every attachment.
    private func prepare(_ review: PendingReview, _ state: inout State) -> Effect<Action> {
        guard state.canReview else { return .none }
        if !state.attachmentPicker.allReady {
            state.pendingReview = review
            return .send(.attachmentPicker(.prepareForSubmission))
        }
        return submit(review, &state)
    }

    private func submit(_ review: PendingReview, _ state: inout State) -> Effect<Action> {
        let id = state.subject.interaction.id
        let attachments = state.attachmentPicker.stored
        let intent: DomainIntent
        switch review {
        case .feedback(let body): intent = .submitFeedback(id, body: body, attachments: attachments)
        case .accept: intent = .acceptDelivery(id)
        case .reply(let body): intent = .replyToInformation(id, body: body, attachments: attachments)
        case let .answer(option, text):
            guard let spec = state.subject.question?.option(option) else { return .none }
            do {
                intent = .answer(id, optionID: option, inputs: try AnswerForm(option: spec).inputs(text: text), attachments: attachments)
            } catch let error as DomainIntentError {
                state.noticeIsSubmissionFailure = false
                state.notice = error.formMessage
                return .none
            } catch { return .none }
        case .moreContext(let body): intent = .requestMoreContext(id, body: body, attachments: attachments)
        }
        state.isSubmitting = true
        state.noticeIsSubmissionFailure = false
        state.notice = nil
        state.conflict = nil
        let revision = state.revision
        let workspace = state.workspace
        if state.commandReview != review { state.commandID = nil }
        let commandID = state.commandID ?? uuid().uuidString.lowercased()
        state.commandID = commandID
        state.commandReview = review
        return .run { send in
            await send(.submissionFinished(revision, await client.perform(intent, workspace, [], commandID)))
        }.cancellable(id: CancelID.submit)
    }
}

public struct DocumentLoadFailure: Error, Equatable, Sendable {
    public let message: String
    public init(message: String) { self.message = message }
}
