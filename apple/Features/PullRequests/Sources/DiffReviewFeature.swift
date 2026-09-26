import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

public enum PRDiffLoadError: Error, Equatable, Sendable {
    case responseTooLarge
}

@Reducer
public struct DiffReviewFeature {
    @ObservableState
    public struct State: Equatable {
        public var attachmentPicker = AttachmentFeature.State()
        public let prID: UInt64
        public var files: [DiffFile]?
        /// Exact raw diff text is the revision token; path:line alone cannot identify content.
        public var loadedHeadSHA: String?
        public var commentsHeadSHA: String?
        public var loadedDiffText: String?
        public var commentsDiffText: String?
        public var collapsed: Set<String> = []
        public var isLoading = false
        public var loadError: String?
        public var isOpen = true
        public var isStale = false
        public var comments: [String: String] = [:]
        public var note = ""
        public var attachments: [PRReviewAttachment] = []
        public var submittedComments: [String: String]?
        public var submittedNote: String?
        public var submittedAttachments: [PRReviewAttachment]?
        public var editingAnchor: String?
        public var editingText = ""
        public var isSending = false
        public var mutationOutcome: PRMutationOutcome?
        public var validationError: String?
        public var shouldDismiss = false
        public var successfulActionCount = 0
        public var presentationEpoch: UInt64 = 0
        public var loadGeneration: UInt64 = 0
        public let loadID = UUID()
        public let mutationID = UUID()

        public init(prID: UInt64) { self.prID = prID }

        public var canReview: Bool {
            attachmentPicker.allReady && isOpen && !isStale && !isLoading && !isSending && files != nil && loadedDiffText != nil
                && loadedHeadSHA != nil
                && mutationOutcome?.uncertainID == nil && mutationOutcome != .confirmed
        }
        public var hasDraft: Bool {
            !comments.isEmpty || !note.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty
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
        case pullRequestChanged(PullRequest?)
        case snapshotUnavailable
        case diffLoaded([DiffFile], PRDiffSnapshot, UInt64)
        case diffFailed(String, UInt64)
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
        case attachmentsChanged([PRReviewAttachment])
        case askTapped
        case requestChangesTapped
        case reviewDiffChanged([DiffFile], PRDiffSnapshot, UInt64)
        case reviewPreflightFailed(String, UInt64)
        case reviewFinished(PRMutationOutcome, UInt64)
        case commandResolved(UUID, rejection: String?)
        case commandReconciled(UUID)
    }

    @Dependency(\.pullRequestOperations) var operations
    private let prepareReview = PreparePRReviewUseCase()

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.attachmentPicker, action: \.attachmentPicker) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .attachmentPicker:
                state.attachments = state.attachmentPicker.entries.map { PRReviewAttachment(id: $0.id, name: $0.draft.name, data: $0.draft.data, storedID: $0.storedID) }
                return .none
            case .appeared, .retryTapped:
                guard state.isOpen, !state.isLoading else { return .none }
                state.isLoading = true
                state.loadError = nil
                let id = state.prID
                state.loadGeneration &+= 1
                let generation = state.loadGeneration
                return .run { send in
                    do {
                        let text = try await operations.diff(id)
                        await send(.diffLoaded(UnifiedDiff.parse(text.diff), text, generation))
                    } catch PRDiffLoadError.responseTooLarge {
                        await send(.diffFailed("This diff is too large to review on this device", generation))
                    } catch where !error.isCancellation {
                        await send(.diffFailed(error.localizedDescription, generation))
                    }
                }.cancellable(id: state.loadID, cancelInFlight: true)
            case .disappeared:
                state.loadGeneration &+= 1
                state.isLoading = false
                return .cancel(id: state.loadID)
            case .identityChanged:
                state.attachmentPicker = AttachmentFeature.State()
                state.presentationEpoch &+= 1
                state.loadGeneration &+= 1
                state.files = nil
                state.loadedDiffText = nil
                state.loadedHeadSHA = nil
                state.commentsDiffText = nil
                state.commentsHeadSHA = nil
                state.collapsed = []
                state.isLoading = false
                state.loadError = nil
                state.isOpen = false
                state.isStale = false
                state.comments = [:]
                state.note = ""
                state.attachments = []
                state.submittedComments = nil
                state.submittedNote = nil
                state.submittedAttachments = nil
                state.editingAnchor = nil
                state.editingText = ""
                state.isSending = false
                state.mutationOutcome = nil
                state.validationError = nil
                state.shouldDismiss = false
                return .merge(.cancel(id: state.loadID), .cancel(id: state.mutationID))
            case .pullRequestChanged(let pr):
                guard pr?.id == state.prID || pr == nil else { return .none }
                state.isOpen = pr?.state == "open"
                state.isStale = false
                if !state.isOpen { state.validationError = "This PR is no longer open" }
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
                state.collapsed = Set(files.filter(\.startsCollapsed).map(\.id))
                state.loadError = nil
                return .none
            case .diffFailed(let error, let generation):
                guard generation == state.loadGeneration, state.isLoading else { return .none }
                state.isLoading = false
                state.loadError = error
                state.files = nil
                state.loadedDiffText = nil
                state.loadedHeadSHA = nil
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
                guard state.editingAnchor != nil, !state.isSending, state.mutationOutcome?.uncertainID == nil else { return .none }
                state.editingText = text
                return .none
            case .saveEditedLine:
                guard let anchor = state.editingAnchor, !state.isSending, state.mutationOutcome?.uncertainID == nil else { return .none }
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
                state.validationError = nil
                return .none
            case .cancelLineComment:
                state.editingText = ""
                state.editingAnchor = nil
                return .none
            case .noteChanged(let note):
                guard !state.isSending else { return .none }
                state.note = note
                return .none
            case .attachmentsChanged(let attachments):
                guard !state.isSending else { return .none }
                state.attachments = attachments
                return .none
            case .askTapped:
                return sendReview(&state, kind: .ask)
            case .requestChangesTapped:
                return sendReview(&state, kind: .requestChanges)
            case .reviewDiffChanged(let files, let text, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.loadGeneration &+= 1
                state.isLoading = false
                state.files = files
                state.loadedDiffText = text.diff
                state.loadedHeadSHA = text.headSHA
                state.collapsed = Set(files.filter(\.startsCollapsed).map(\.id))
                state.validationError = "The PR changed; review the new diff before sending"
                state.submittedComments = nil
                state.submittedNote = nil
                state.submittedAttachments = nil
                return .cancel(id: state.loadID)
            case .reviewPreflightFailed(let message, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.validationError = message
                state.submittedComments = nil
                state.submittedNote = nil
                state.submittedAttachments = nil
                return .none
            case .reviewFinished(let outcome, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.mutationOutcome = outcome
                switch outcome {
                case .confirmed:
                    for (anchor, text) in state.submittedComments ?? [:] where state.comments[anchor] == text {
                        state.comments[anchor] = nil
                    }
                    if state.comments.isEmpty { state.commentsDiffText = nil; state.commentsHeadSHA = nil }
                    if state.note == state.submittedNote { state.note = "" }
                    if state.attachments == state.submittedAttachments {
                        state.attachments = []
                        state.attachmentPicker = AttachmentFeature.State()
                    }
                    state.successfulActionCount += 1
                    // An edit made while the request was in flight is a new draft. Keep the
                    // screen and let the user decide whether to send it separately.
                    state.shouldDismiss = !state.hasDraft
                    if state.hasDraft { state.mutationOutcome = nil }
                case .closed:
                    state.isOpen = false
                    state.validationError = "This PR is no longer open"
                case .rejected(let message):
                    state.validationError = message
                case .unknown, .obsolete:
                    break
                }
                if outcome.uncertainID == nil {
                    state.submittedComments = nil
                    state.submittedNote = nil
                    state.submittedAttachments = nil
                }
                return .none
            case .commandResolved(let id, let rejection):
                guard state.mutationOutcome?.uncertainID == id else { return .none }
                state.isSending = true
                return .send(.reviewFinished(rejection.map(PRMutationOutcome.rejected) ?? .confirmed, state.presentationEpoch))
            case .commandReconciled(let id):
                if state.mutationOutcome?.uncertainID == id { state.mutationOutcome = nil }
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in
                AttachmentFeature.cancelEffects(instanceID: oldID).map { .attachmentPicker($0) }
            }
        }
    }

    private func sendReview(_ state: inout State, kind: PRReviewKind) -> Effect<Action> {
        guard state.canReview else { return .none }
        switch prepareReview.execute(comments: state.comments, note: state.note,
                                     attachments: state.attachments, files: state.files,
                                     isOpen: state.isOpen, commentsAreCurrent: !state.commentsNeedReview) {
        case .empty:
            state.validationError = "Add a note, line comment, or attachment"
            return .none
        case .closed:
            state.validationError = "This PR is no longer open"
            return .none
        case .outdatedDiff:
            state.validationError = "The diff changed; refresh before sending line comments"
            return .none
        case .textRequired:
            state.validationError = "Add a note or line comment before sending attachments"
            return .none
        case .ready(let text):
            guard let expectedDiff = state.loadedDiffText, let expectedHead = state.loadedHeadSHA else { return .none }
            state.isSending = true
            state.validationError = nil
            state.mutationOutcome = nil
            state.submittedComments = state.comments
            state.submittedNote = state.note
            state.submittedAttachments = state.attachments
            let id = state.prID
            let attachments = state.attachments
            let epoch = state.presentationEpoch
            return .run { send in
                do {
                    let currentDiff = try await operations.diff(id)
                    try Task.checkCancellation()
                    guard currentDiff.diff == expectedDiff, currentDiff.headSHA == expectedHead else {
                        await send(.reviewDiffChanged(UnifiedDiff.parse(currentDiff.diff), currentDiff, epoch))
                        return
                    }
                    await send(.reviewFinished(await operations.review(id, text, attachments, kind, expectedHead), epoch))
                } catch PRDiffLoadError.responseTooLarge {
                    await send(.reviewPreflightFailed("This diff is too large to verify; review was not sent", epoch))
                } catch where !error.isCancellation {
                    await send(.reviewPreflightFailed("Could not verify the latest diff; review was not sent: \(error.localizedDescription)", epoch))
                }
            }.cancellable(id: state.mutationID)
        }
    }
}
