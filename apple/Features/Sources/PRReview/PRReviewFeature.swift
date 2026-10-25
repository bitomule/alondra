import AlondraContract
import AlondraDesign
import AlondraFeed
import ComposableArchitecture
import Foundation

/// One pull request: its description and status, the diff with line comments, and the ways to act
/// on it (merge by method, resolve conflicts, ask, request changes). Questions and reports the
/// agent attached to the PR are answered here too, through the same action sheet.
@Reducer
public struct PRReviewFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public enum Diff: Equatable, Sendable {
            case unavailable
            case loading
            case loaded([DiffFile])
            case failed(String)

            public var files: [DiffFile]? { if case .loaded(let files) = self { files } else { nil } }
            public var failed: Bool { if case .failed = self { true } else { false } }
        }

        public var screen: ScreenFeature.State
        public var actions: ActionFeature.State
        public var diff = Diff.unavailable
        public var diffDigest: String?
        public var comments: [String: ActionComment] = [:]
        public var collapsed: Set<String> = []
        public var diffTruncated = false
        public var showsChanges = false
        public var attachment: AttachmentPreview?
        public var failure: String?

        public init(key: ViewKey) {
            screen = ScreenFeature.State(key: key)
            actions = .ios()
            actions.handsOff = true
        }

        public var review: PRReview? { screen.document.map(PRReview.init(document:)) }

        /// The sheet shows for answers that take text or a choice; a merge or a conflict hand-off
        /// is one tap and only ever asks for confirmation.
        public var isComposing: Bool { actions.action?.needsSheet == true }

        public var sortedComments: [ActionComment] { comments.values.sorted { $0.anchor < $1.anchor } }
    }

    public enum Action {
        case task
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case diffLoaded(digest: String?, [DiffFile], truncated: Bool = false)
        case changesShown(Bool)
        case toggleFile(String)
        case toggleAllFiles
        case diffFailed(digest: String?, String)
        case retryDiff
        case perform(WireAction, optionID: String? = nil)
        case comment(anchor: String, ActionComment?)
        case dismissSheet
        case follow(AlondraContract.Link)
        case showAttachment(AlondraContract.Attachment)
        case attachmentLoaded(AlondraContract.Attachment, Data)
        case attachmentFailed(String)
        case closeAttachment
        case dismissFailure
        case delegate(ScreenDelegate)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.dismiss) var dismiss
    private enum CancelID { case diff }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .retryDiff:
                guard let ref = state.review?.diff else { return .none }
                state.diff = .loading
                return fetchDiff(ref)
            case .screen(.loaded(let document)):
                state.actions.dataEpoch = document.dataEpoch
                guard let ref = PRReview(document: document).diff else {
                    state.diff = .unavailable
                    state.diffDigest = nil
                    return .cancel(id: CancelID.diff)
                }
                guard state.diff == .unavailable || ref.digest != state.diffDigest else { return .none }
                state.diffDigest = ref.digest
                state.comments = [:]
                state.diff = .loading
                return fetchDiff(ref)
            case .screen:
                return .none
            case .diffLoaded(let digest, let files, let truncated):
                guard digest == state.diffDigest else { return .none }
                state.diff = .loaded(files)
                state.diffTruncated = truncated
                state.collapsed = Set(files.filter(\.startsCollapsed).map(\.id))
                return .none
            case .changesShown(let shown):
                state.showsChanges = shown
                return .none
            case .toggleFile(let id):
                state.collapsed.formSymmetricDifference([id])
                return .none
            case .toggleAllFiles:
                guard case .loaded(let files) = state.diff else { return .none }
                state.collapsed = state.collapsed.count == files.count ? [] : Set(files.map(\.id))
                return .none
            case .diffFailed(let digest, let message):
                guard digest == state.diffDigest else { return .none }
                state.diff = .failed(message)
                return .none
            case .comment(let anchor, let comment):
                state.comments[anchor] = comment
                return .none
            case .perform(let wire, let optionID):
                guard wire.enabled else { return .none }
                var effects: [Effect<Action>] = [.send(.actions(.present(wire)))]
                if wire.comments?.kind == .diffLines {
                    effects.append(.send(.actions(.commentsChanged(state.sortedComments))))
                }
                if let optionID { effects.append(.send(.actions(.chooseOption(optionID)))) }
                if !wire.needsSheet { effects.append(.send(.actions(.submit))) }
                return .concatenate(effects)
            case .dismissSheet:
                return .send(.actions(.dismiss))
            case .actions(.sent(let request)):
                return .merge(.send(.delegate(.handOff(request))), .run { _ in await dismiss() })
            case .actions(.delegate(let delegate)):
                return .send(.delegate(ScreenDelegate(delegate)))
            case .actions:
                return .none
            case .follow(let link):
                guard let open = ScreenDelegate.opening(link) else { return .none }
                return .send(.delegate(open))
            case .showAttachment(let attachment):
                return .run { [viewsClient] send in
                    do { await send(.attachmentLoaded(attachment, try await viewsClient.fetchAttachment(attachment))) }
                    catch { await send(.attachmentFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
                }
            case .attachmentLoaded(let attachment, let data):
                state.attachment = AttachmentPreview(attachment: attachment, data: data)
                return .none
            case .attachmentFailed(let message):
                state.failure = message
                return .none
            case .closeAttachment:
                state.attachment = nil
                return .none
            case .dismissFailure:
                state.failure = nil
                return .none
            case .delegate:
                return .none
            }
        }
    }

    private func fetchDiff(_ ref: ContentRef) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                let fetched = try await viewsClient.fetchDiff(ref)
                await send(.diffLoaded(digest: ref.digest, UnifiedDiff.parse(fetched.text), truncated: fetched.truncated))
            }
            catch is CancellationError { return }
            catch { await send(.diffFailed(digest: ref.digest, String(localized: "Couldn’t load the diff. Try again."))) }
        }
        .cancellable(id: CancelID.diff, cancelInFlight: true)
    }
}

private extension WireAction {
    var needsSheet: Bool {
        if case .text = input { return true }
        return presentation == .options
    }
}
