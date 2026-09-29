import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

/// Reviews the presented head's diff. Requesting changes is one `submit_feedback` on the
/// presentation; asking is `ask_about_pull_request` and leaves the review open.
@Reducer
public struct DiffReviewFeature {
    public enum ReviewKind: Equatable, Sendable { case ask, requestChanges }

    @ObservableState
    public struct State: Equatable {
        public var attachmentPicker = AttachmentFeature.State()
        public let prID: DomainV2.PrId
        public var card: DomainPullRequestCard?
        public var workspace: DomainWorkspace?
        public var files: [DiffFile]?
        /// Exact raw diff text is the revision token; path:line alone cannot identify content.
        public var loadedHeadSHA: String?
        public var commentsHeadSHA: String?
        public var loadedDiffText: String?
        public var diffTruncated = false
        public var commentsDiffText: String?
        public var collapsed: Set<String> = []
        public var highlightedLines: [String: AttributedString] = [:]
        public var highlightingDark: Bool?
        public var highlightGeneration: UInt64 = 0
        public let highlightID = UUID()
        public var isLoading = false
        public var loadError: String?
        public var isOpen = true
        public var isStale = false
        public var comments: [String: String] = [:]
        public var note = ""
        public var submittedComments: [String: String]?
        public var submittedNote: String?
        public var editingAnchor: String?
        public var editingText = ""
        public var isSending = false
        /// Waiting for attachment uploads before sending; never a retry of a sent command.
        public var pendingReviewKind: ReviewKind?
        /// The command ID of an attempt whose outcome is unknown; sending the same review again reuses it.
        public var commandID: String?
        public var commandKind: ReviewKind?
        public var mutationOutcome: PRMutationOutcome?
        public var validationError: String?
        public var validationErrorIsSubmissionFailure = false
        public var shouldDismiss = false
        public var successfulActionCount = 0
        public var presentationEpoch: UInt64 = 0
        public var loadGeneration: UInt64 = 0
        public let loadID = UUID()
        public let mutationID = UUID()

        public mutating func cancelPendingSubmission() { pendingReviewKind = nil }

        public init(card: DomainPullRequestCard, workspace: DomainWorkspace) {
            prID = card.id
            self.card = card
            self.workspace = workspace
            isOpen = card.isOpen
        }

        /// Feedback belongs to the presentation; a PR without one takes no review.
        public var canReview: Bool {
            card?.canSendFeedback == true && canAsk
        }
        public var canAsk: Bool {
            attachmentPicker.canPrepareSubmission && pendingReviewKind == nil && isOpen && !isStale && !isLoading
                && !isSending && mutationOutcome != .confirmed
        }
        public var hasDraft: Bool {
            !comments.isEmpty || !note.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachmentPicker.entries.isEmpty
        }
        public var commentsNeedReview: Bool {
            !comments.isEmpty && (loadedDiffText == nil || commentsDiffText != loadedDiffText || commentsHeadSHA != loadedHeadSHA)
        }
    }

    public enum Action {
        case attachmentPicker(AttachmentFeature.Action)
        case appeared
        case retryTapped
        case disappeared
        case identityChanged
        case cardChanged(DomainPullRequestCard?, DomainWorkspace?)
        case snapshotUnavailable
        case diffLoaded([DiffFile], PRDiffSnapshot, UInt64)
        case diffFailed(String, UInt64)
        case colorSchemeChanged(dark: Bool)
        case highlighted([String: AttributedString], UInt64)
        case toggleFile(String)
        case toggleAllFiles
        case editLine(String)
        case lineEditorChanged(String)
        case saveEditedLine
        case saveLineComment(String, String)
        case removeLineComment(String)
        case confirmCommentsForCurrentDiff
        case cancelLineComment
        case noteChanged(String)
        case askTapped
        case requestChangesTapped
        case reviewDiffChanged([DiffFile], PRDiffSnapshot, UInt64)
        case reviewPreflightFailed(String, UInt64)
        case reviewFinished(PRMutationOutcome, UInt64)
    }

    @Dependency(\.domainWorkspace) var client
    @Dependency(\.uuid) var uuid
    @Dependency(\.diffHighlighting) var highlighting
    private let prepareReview = PreparePRReviewUseCase()

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.attachmentPicker, action: \.attachmentPicker) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .attachmentPicker(let action):
                if action.invalidatesPendingSubmission {
                    state.cancelPendingSubmission()
                    state.commandID = nil
                    return .none
                }
                guard let kind = state.pendingReviewKind else { return .none }
                if case .uploaded(_, .failure(let error)) = action {
                    state.cancelPendingSubmission()
                    state.validationErrorIsSubmissionFailure = true
                    state.validationError = error.message
                    return .none
                }
                guard state.attachmentPicker.allReady else { return .none }
                state.cancelPendingSubmission()
                return sendReview(&state, kind: kind)
            case .appeared, .retryTapped:
                guard state.isOpen, !state.isLoading, let head = state.card?.presentedHead else { return .none }
                state.isLoading = true
                state.loadError = nil
                let id = state.prID
                state.loadGeneration &+= 1
                let generation = state.loadGeneration
                return .run { send in
                    do {
                        let text = PRDiffSnapshot(try await client.pullRequestDiff(id, head))
                        await send(.diffLoaded(UnifiedDiff.parse(text.diff), text, generation))
                    } catch PRDiffLoadError.responseTooLarge {
                        await send(.diffFailed("This diff is too large to review on this device", generation))
                    } catch where !error.isCancellation {
                        await send(.diffFailed(error.localizedDescription, generation))
                    }
                }.cancellable(id: state.loadID, cancelInFlight: true)
            case .colorSchemeChanged(let dark):
                state.highlightingDark = dark
                return highlight(&state)
            case .highlighted(let lines, let generation):
                guard generation == state.highlightGeneration else { return .none }
                state.highlightedLines = lines
                return .none
            case .disappeared:
                state.highlightGeneration &+= 1
                state.loadGeneration &+= 1
                state.isLoading = false
                return .merge(.cancel(id: state.loadID), .cancel(id: state.highlightID))
            case .identityChanged:
                state.attachmentPicker = AttachmentFeature.State()
                state.presentationEpoch &+= 1
                state.loadGeneration &+= 1
                state.files = nil
                state.highlightedLines = [:]
                state.highlightGeneration &+= 1
                state.loadedDiffText = nil
                state.loadedHeadSHA = nil
                state.diffTruncated = false
                state.commentsDiffText = nil
                state.commentsHeadSHA = nil
                state.collapsed = []
                state.isLoading = false
                state.loadError = nil
                state.isOpen = false
                state.isStale = false
                state.comments = [:]
                state.note = ""
                state.submittedComments = nil
                state.submittedNote = nil
                state.editingAnchor = nil
                state.editingText = ""
                state.isSending = false
                state.mutationOutcome = nil
                state.validationErrorIsSubmissionFailure = false
                state.validationError = nil
                state.shouldDismiss = false
                return .merge(.cancel(id: state.loadID), .cancel(id: state.mutationID))
            case let .cardChanged(card, workspace):
                guard card?.id == state.prID || card == nil else { return .none }
                let previousHead = state.card?.presentedHead
                state.card = card
                state.workspace = workspace
                state.isOpen = card?.isOpen == true
                state.isStale = false
                if !state.isOpen {
                    state.validationErrorIsSubmissionFailure = false
                    state.validationError = String(localized: "This PR is no longer open")
                    return .none
                }
                // A new presented head is a new diff: reload it. Comments made on the old one
                // stay as a draft that must be confirmed against the new diff before sending.
                if let head = card?.presentedHead, head != previousHead, state.files != nil, !state.isLoading {
                    return .send(.retryTapped)
                }
                return .none
            case .snapshotUnavailable:
                state.isStale = true
                return .none
            case .diffLoaded(let files, let text, let generation):
                guard generation == state.loadGeneration, state.isLoading else { return .none }
                state.isLoading = false
                state.files = files
                state.loadedDiffText = text.diff
                state.loadedHeadSHA = text.headSHA
                state.diffTruncated = text.truncated
                state.collapsed = Set(files.filter(\.startsCollapsed).map(\.id))
                state.loadError = nil
                return highlight(&state)
            case .diffFailed(let error, let generation):
                guard generation == state.loadGeneration, state.isLoading else { return .none }
                state.isLoading = false
                state.loadError = error
                state.files = nil
                state.highlightedLines = [:]
                state.highlightGeneration &+= 1
                state.loadedDiffText = nil
                state.loadedHeadSHA = nil
                state.diffTruncated = false
                state.collapsed = []
                return .none
            case .toggleFile(let id):
                guard state.files?.contains(where: { $0.id == id }) == true else { return .none }
                state.collapsed.formSymmetricDifference([id])
                return .none
            case .toggleAllFiles:
                guard let files = state.files else { return .none }
                let ids = Set(files.map(\.id))
                state.collapsed = state.collapsed == ids ? [] : ids
                return .none
            case .editLine(let anchor):
                guard state.files?.contains(where: { $0.lines.contains(where: { $0.anchor == anchor }) }) == true else { return .none }
                state.editingAnchor = anchor
                state.editingText = state.comments[anchor] ?? ""
                return .none
            case .lineEditorChanged(let text):
                guard state.editingAnchor != nil, !state.isSending else { return .none }
                state.editingText = text
                return .none
            case .saveEditedLine:
                guard let anchor = state.editingAnchor, !state.isSending else { return .none }
                state.commandID = nil
                return .send(.saveLineComment(anchor, state.editingText))
            case .saveLineComment(let anchor, let text):
                guard state.editingAnchor == anchor,
                      state.files?.contains(where: { $0.lines.contains(where: { $0.anchor == anchor }) }) == true else { return .none }
                let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
                if state.comments.isEmpty, !trimmed.isEmpty { state.commentsDiffText = state.loadedDiffText; state.commentsHeadSHA = state.loadedHeadSHA }
                state.comments[anchor] = trimmed.isEmpty ? nil : trimmed
                if state.comments.isEmpty { state.commentsDiffText = nil; state.commentsHeadSHA = nil }
                state.editingAnchor = nil
                state.editingText = ""
                return .none
            case .removeLineComment(let anchor):
                state.comments[anchor] = nil
                if state.comments.isEmpty { state.commentsDiffText = nil; state.commentsHeadSHA = nil }
                if state.editingAnchor == anchor { state.editingAnchor = nil }
                return .none
            case .confirmCommentsForCurrentDiff:
                guard let files = state.files, let revision = state.loadedDiffText else { return .none }
                let anchors = Set(files.flatMap(\.lines).compactMap(\.anchor))
                guard state.comments.keys.allSatisfy(anchors.contains) else { return .none }
                state.commentsDiffText = revision
                state.commentsHeadSHA = state.loadedHeadSHA
                state.validationErrorIsSubmissionFailure = false
                state.validationError = nil
                return .none
            case .cancelLineComment:
                state.editingText = ""
                state.editingAnchor = nil
                return .none
            case .noteChanged(let note):
                guard !state.isSending else { return .none }
                if state.note != note { state.commandID = nil }
                state.note = note
                return .none
            case .askTapped:
                return prepare(&state, kind: .ask)
            case .requestChangesTapped:
                return prepare(&state, kind: .requestChanges)
            case .reviewDiffChanged(let files, let text, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.loadGeneration &+= 1
                state.isLoading = false
                state.files = files
                state.loadedDiffText = text.diff
                state.loadedHeadSHA = text.headSHA
                state.diffTruncated = text.truncated
                state.collapsed = Set(files.filter(\.startsCollapsed).map(\.id))
                state.validationErrorIsSubmissionFailure = false
                state.validationError = String(localized: "The PR changed; review the new diff before sending")
                state.submittedComments = nil
                state.submittedNote = nil
                return .merge(.cancel(id: state.loadID), highlight(&state))
            case .reviewPreflightFailed(let message, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.validationErrorIsSubmissionFailure = false
                state.validationError = message
                state.submittedComments = nil
                state.submittedNote = nil
                return .none
            case .reviewFinished(let outcome, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.mutationOutcome = outcome
                switch outcome {
                case .confirmed:
                    if state.attachmentPicker.allReady { state.attachmentPicker = AttachmentFeature.State() }
                    for (anchor, text) in state.submittedComments ?? [:] where state.comments[anchor] == text {
                        state.comments[anchor] = nil
                    }
                    if state.comments.isEmpty { state.commentsDiffText = nil; state.commentsHeadSHA = nil }
                    if state.note == state.submittedNote { state.note = "" }
                    state.successfulActionCount += 1
                    // An edit made while the request was in flight is a new draft. Keep the
                    // screen and let the user decide whether to send it separately.
                    state.shouldDismiss = !state.hasDraft
                    if state.hasDraft { state.mutationOutcome = nil }
                case .closed:
                    state.isOpen = false
                    state.validationErrorIsSubmissionFailure = false
                    state.validationError = String(localized: "This PR is no longer open")
                case .rejected(let message):
                    state.validationErrorIsSubmissionFailure = true
                    state.validationError = message
                case .unknown, .obsolete:
                    break
                }
                if outcome != .unknown {
                    state.commandID = nil
                    state.submittedComments = nil
                    state.submittedNote = nil
                }
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in
                AttachmentFeature.cancelEffects(instanceID: oldID).map { .attachmentPicker($0) }
            }
        }
    }

    private func highlight(_ state: inout State) -> Effect<Action> {
        state.highlightGeneration &+= 1
        state.highlightedLines = [:]
        let generation = state.highlightGeneration
        let files = state.files ?? []
        guard let dark = state.highlightingDark, !files.isEmpty else { return .cancel(id: state.highlightID) }
        return .run { send in
            let lines = await highlighting.render(files, dark)
            try Task.checkCancellation()
            await send(.highlighted(lines, generation))
        }.cancellable(id: state.highlightID, cancelInFlight: true)
    }

    /// Uploads finish before the command is built, so it carries every attachment.
    private func prepare(_ state: inout State, kind: ReviewKind) -> Effect<Action> {
        guard kind == .ask ? state.canAsk : state.canReview else { return .none }
        if !state.attachmentPicker.allReady {
            state.pendingReviewKind = kind
            return .send(.attachmentPicker(.prepareForSubmission))
        }
        return sendReview(&state, kind: kind)
    }

    private func sendReview(_ state: inout State, kind: ReviewKind) -> Effect<Action> {
        guard kind == .ask ? state.canAsk : state.canReview, let card = state.card, let workspace = state.workspace else { return .none }
        switch prepareReview.execute(comments: state.comments, note: state.note, files: state.files,
                                     isOpen: state.isOpen, commentsAreCurrent: !state.commentsNeedReview) {
        case .empty:
            state.validationErrorIsSubmissionFailure = false
            state.validationError = String(localized: "Add a note or line comment")
            return .none
        case .closed:
            state.validationErrorIsSubmissionFailure = false
            state.validationError = String(localized: "This PR is no longer open")
            return .none
        case .outdatedDiff:
            state.validationErrorIsSubmissionFailure = false
            state.validationError = String(localized: "The diff changed; refresh before sending line comments")
            return .none
        case .ready(let text):
            let expected: PRDiffSnapshot? = if let diff = state.loadedDiffText, let head = state.loadedHeadSHA {
                PRDiffSnapshot(diff: diff, headSHA: head, truncated: state.diffTruncated)
            } else { nil }
            state.isSending = true
            state.validationErrorIsSubmissionFailure = false
            state.validationError = nil
            state.mutationOutcome = nil
            state.submittedComments = state.comments
            state.submittedNote = state.note
            let epoch = state.presentationEpoch
            let attachments = state.attachmentPicker.stored
            if state.commandKind != kind { state.commandID = nil }
            let commandID = state.commandID ?? uuid().uuidString.lowercased()
            state.commandID = commandID
            state.commandKind = kind
            if kind == .ask {
                let head = state.loadedHeadSHA ?? card.presentedHead ?? card.pullRequest.head
                return .run { send in
                    let result = await client.perform(.askAboutPullRequest(card.id, head: head, body: text, attachments: attachments),
                                                      workspace, [], commandID)
                    await send(.reviewFinished(PRMutationOutcome(result), epoch))
                }.cancellable(id: state.mutationID)
            }
            let useCase = SubmitPRReviewUseCase(client: client)
            return .run { send in
                do {
                    switch try await useCase.execute(card: card, text: text, attachments: attachments, expected: expected,
                                                     in: workspace, commandID: commandID) {
                    case .diffChanged(let current):
                        await send(.reviewDiffChanged(UnifiedDiff.parse(current.diff), current, epoch))
                    case .finished(let result):
                        await send(.reviewFinished(PRMutationOutcome(result), epoch))
                    }
                } catch PRDiffLoadError.responseTooLarge {
                    await send(.reviewPreflightFailed(String(localized: "This diff is too large to verify; review was not sent"), epoch))
                } catch where !error.isCancellation {
                    await send(.reviewPreflightFailed(String(localized: "Could not verify the latest diff; review was not sent: \(error.localizedDescription)"), epoch))
                }
            }.cancellable(id: state.mutationID)
        }
    }
}
