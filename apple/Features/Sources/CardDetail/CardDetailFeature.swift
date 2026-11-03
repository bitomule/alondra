import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeed
import ComposableArchitecture
import Foundation

/// A card opened in full, whatever it is: the sections the daemon sends (content, context,
/// history) and the same actions the card has, pinned at the bottom with the reply field. The text
/// of a document or a plan can be commented block by block; the comments, the reply and the attachments
/// live only while the detail is open and are cleared when one of the actions is sent.
@Reducer
public struct CardDetailFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var actions = ActionFeature.State.device()
        public var attachments = AttachmentFeature.State()
        public var content: String?
        public var contentDigest: String?
        public var contentFailed = false
        public var diff: [DiffFile]?
        public var diffDigest: String?
        public var diffFailed = false
        public var diffTruncated = false
        public var collapsedFiles: Set<String> = []
        public var viewed = ViewedFilesFeature.State()
        public var showsChanges = false
        public var comments: [String: ActionComment] = [:]
        public var reply = ""
        public var composing: Composing?
        public var composition: TriageCardFeature.Composition?
        public var awaitingAttachments = false
        /// What the person filled in the MCP form the card carries, starting from its defaults.
        public var formValues: [String: JSONValue] = [:]
        public var form: RequestedForm?

        public init(key: ViewKey) {
            screen = ScreenFeature.State(key: key)
            actions.handsOff = true
        }

        public var subject: Subject? { Subject(document: screen.document, key: screen.key) }
        public var contentPending: Bool { contentDigest != nil && content == nil && !contentFailed }
        public var markdown: String { content ?? "" }
        public var blocks: [MarkdownBlock] { MarkdownParser.blocks(from: markdown) }
        public var feedback: [ActionComment] { blocks.compactMap { comments[$0.anchor] } }
        public var canComment: Bool { subject?.allActions.contains { $0.comments?.kind == .blocks } == true }
        public var hasFeedback: Bool {
            !comments.isEmpty || !reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                || !attachments.entries.isEmpty
        }

        public func isSendable(_ action: WireAction) -> Bool {
            guard action.enabled else { return false }
            let needsFeedback = action.comments != nil || action.id?.hasPrefix("reply:") == true
            return !needsFeedback || hasFeedback
        }
    }

    /// What the detail shows, whether the daemon sent a card or a document revision.
    public struct Subject: Equatable, Sendable {
        /// An action bar shows at most three buttons; the rest go behind "More…".
        static let barLimit = 3

        public let card: Card?
        public let title: String
        public let sections: [ViewSection]
        public let buttons: [WireAction]
        public let menu: [WireAction]
        public let replyAction: WireAction?

        init?(document: ViewDocument?, key: ViewKey) {
            guard let document else { return nil }
            let card = document.sections.lazy.flatMap(\.items).compactMap { item -> Card? in
                if case .card(let card) = item { card } else { nil }
            }.first
            self.card = card
            title = card?.title ?? card?.need ?? document.title ?? ""
            sections = document.sections.filter { $0.id != "card" && $0.id != "actions" }
            if let card {
                buttons = card.actions
                menu = card.more
                replyAction = card.reply
            } else {
                let bar = document.sections.flatMap(\.items).flatMap { item -> [WireAction] in
                    if case .actions(let bar) = item { bar.actions } else { [] }
                }
                buttons = Array(bar.prefix(Self.barLimit))
                menu = Array(bar.dropFirst(Self.barLimit))
                replyAction = nil
            }
        }

        public var allActions: [WireAction] { buttons + menu + (replyAction.map { [$0] } ?? []) }

        /// The view that opens the diff a `content_ref` points at: the pull request's own screen.
        public static func diffKey(of reference: ContentRef) -> ViewKey? {
            let prefix = "/pull-requests/"
            guard reference.kind == .diff, reference.fetch.hasPrefix(prefix) else { return nil }
            let rest = reference.fetch.dropFirst(prefix.count)
            guard let end = rest.range(of: "/diff") else { return nil }
            guard let id = String(rest[..<end.lowerBound]).removingPercentEncoding, !id.isEmpty else { return nil }
            return .pullRequest(id)
        }
    }

    public struct Composing: Equatable, Identifiable, Sendable {
        public let anchor: String
        public let quote: String
        public var text: String
        public var id: String { anchor }

        public init(anchor: String, quote: String, text: String) {
            self.anchor = anchor
            self.quote = quote
            self.text = text
        }
    }

    public enum Action {
        case task
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case attachments(AttachmentFeature.Action)
        case contentLoaded(digest: String?, String)
        case contentFailed
        case diffLoaded(digest: String?, [DiffFile], truncated: Bool = false, marks: FileMarks? = nil)
        case diffFailed(digest: String?)
        case retryDiff
        case toggleFile(String)
        case viewed(ViewedFilesFeature.Action)
        case changesShown(Bool)
        case toggleAllFiles
        case blockTapped(anchor: String, quote: String)
        case composingChanged(String)
        case commentSaved
        case commentDeleted
        case commentCancelled
        case replyChanged(String)
        case formChanged([String: JSONValue])
        case actionTapped(WireAction)
        case menuOptionPicked(WireAction, optionID: String)
        case reasonChanged(String)
        case noteChanged(String)
        case compositionConfirmed
        case compositionCancelled
        case opened(ViewKey)
        case submit
        case loadTimedOut
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case open(ViewKey)
            case handOff(ActionRequest)
            /// The card no longer waits for the user: the screen leaves, saying why.
            case settled(String)
        }
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.dismiss) var dismiss
    @Dependency(\.continuousClock) var clock
    private enum CancelID: Hashable { case loadDeadline }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Scope(state: \.attachments, action: \.attachments) { AttachmentFeature() }
        Scope(state: \.viewed, action: \.viewed) { ViewedFilesFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .merge(.send(.screen(.task)), loadDeadline(state))
            case .screen(.retry):
                return loadDeadline(state)
            case .screen(.stop), .screen(.failed), .screen(.missing):
                return .cancel(id: CancelID.loadDeadline)
            case .loadTimedOut:
                guard state.screen.document == nil, state.screen.phase == .loading else { return .none }
                state.screen.phase = .failed(TriageFeature.loadFailure)
                return .none
            case .screen(.loaded(let document)):
                guard document.key == state.screen.key.rawValue else { return .none }
                if let why = document.settledNotice {
                    return .merge(.cancel(id: CancelID.loadDeadline), .send(.delegate(.settled(why))), .run { [dismiss] _ in await dismiss() })
                }
                state.actions.dataEpoch = document.dataEpoch
                if let form = state.subject?.card?.form, form != state.form {
                    state.form = form
                    state.formValues = form.initialValues
                }
                if let inline = Self.commentableText(of: document) {
                    state.content = inline
                    state.contentDigest = nil
                    state.contentFailed = false
                    return .cancel(id: CancelID.loadDeadline)
                }
                return .merge(.cancel(id: CancelID.loadDeadline), fetchContent(of: document, state: &state), fetchDiff(of: document, state: &state))
            case .diffLoaded(let digest, let files, let truncated, let marks):
                guard digest == state.diffDigest else { return .none }
                state.diff = files
                state.diffTruncated = truncated
                state.diffFailed = false
                state.collapsedFiles = Set(files.filter { $0.startsCollapsed || marks?.files[$0.id] == .viewed }.map(\.id))
                state.viewed = ViewedFilesFeature.State(saved: marks)
                return .none
            case .viewed(.toggle(let path)):
                if state.viewed.isViewed(path) { state.collapsedFiles.insert(path) } else { state.collapsedFiles.remove(path) }
                return .none
            case .viewed(.delegate(.reload)):
                guard let document = state.screen.document else { return .none }
                state.diffDigest = nil
                return fetchDiff(of: document, state: &state)
            case .viewed:
                return .none
            case .diffFailed(let digest):
                guard digest == state.diffDigest else { return .none }
                state.diffFailed = true
                return .none
            case .retryDiff:
                guard let document = state.screen.document else { return .none }
                state.diffDigest = nil
                return fetchDiff(of: document, state: &state)
            case .toggleFile(let id):
                if !state.collapsedFiles.insert(id).inserted { state.collapsedFiles.remove(id) }
                return .none
            case .changesShown(let shown):
                state.showsChanges = shown
                return .none
            case .toggleAllFiles:
                guard let files = state.diff else { return .none }
                state.collapsedFiles = state.collapsedFiles.count == files.count ? [] : Set(files.map(\.id))
                return .none
            case .screen, .delegate:
                return .none
            case .opened(let key):
                return .send(.delegate(.open(key)))
            case .contentLoaded(let digest, let text):
                guard digest == state.contentDigest else { return .none }
                state.content = text
                state.contentFailed = false
                return .none
            case .contentFailed:
                state.contentFailed = true
                return .none
            case .blockTapped(let anchor, let quote):
                state.composing = Composing(anchor: anchor, quote: quote, text: state.comments[anchor]?.text ?? "")
                return .none
            case .composingChanged(let text):
                state.composing?.text = text
                return .none
            case .commentSaved:
                guard let composing = state.composing else { return .none }
                state.composing = nil
                let text = composing.text.trimmingCharacters(in: .whitespacesAndNewlines)
                state.comments[composing.anchor] = text.isEmpty ? nil
                    : ActionComment(anchor: composing.anchor, text: text, quote: composing.quote)
                return .none
            case .commentDeleted:
                guard let composing = state.composing else { return .none }
                state.composing = nil
                state.comments[composing.anchor] = nil
                return .none
            case .commentCancelled:
                state.composing = nil
                return .none
            case .replyChanged(let text):
                state.reply = text
                return .none
            case .formChanged(let values):
                state.formValues = values
                return .none
            case .actionTapped(let action):
                let field = action == state.subject?.replyAction
                if !field, case .view(let key) = action.link { return .send(.opened(ViewKey(rawValue: key))) }
                guard field || action.link == nil, state.isSendable(action) else { return .none }
                if action.comments == nil, action != state.subject?.replyAction, TriageCardFeature.asksForWords(action) {
                    state.composition = TriageCardFeature.Composition(action: action)
                    return .none
                }
                return .concatenate(
                    .send(.actions(.present(action))),
                    state.form.map { .send(.actions(.formFilled($0, state.formValues))) } ?? .none,
                    .send(.actions(.commentsChanged(state.feedback))),
                    .send(.actions(.textChanged(state.reply))),
                    .send(.submit)
                )
            case .menuOptionPicked(let action, let optionID):
                return .concatenate(
                    .send(.actions(.present(action))), .send(.actions(.chooseOption(optionID))), .send(.submit)
                )
            case .reasonChanged(let reason):
                state.composition?.reason = reason
                return .none
            case .noteChanged(let note):
                state.composition?.note = note
                return .none
            case .compositionCancelled:
                state.composition = nil
                return .none
            case .compositionConfirmed:
                guard let composition = state.composition, composition.canSend else { return .none }
                state.composition = nil
                return .concatenate(
                    .send(.actions(.present(composition.action))),
                    composition.reason.map { .send(.actions(.chooseOption($0))) } ?? .none,
                    composition.note.isEmpty ? .none : .send(.actions(.textChanged(composition.note))),
                    .send(.submit)
                )
            case .submit:
                guard state.actions.action != nil else { return .none }
                guard state.attachments.canPrepareSubmission else {
                    state.actions.error = state.attachments.importError
                    return .none
                }
                if !state.attachments.allReady {
                    state.awaitingAttachments = true
                    return .send(.attachments(.prepareForSubmission))
                }
                state.awaitingAttachments = false
                return .concatenate(
                    .send(.actions(.attachmentsChanged(state.attachments.storedIDs))),
                    .send(.actions(.submit))
                )
            case .attachments:
                guard state.awaitingAttachments else { return .none }
                if let failure = state.attachments.failedUploadMessage {
                    state.awaitingAttachments = false
                    state.actions.error = failure
                    return .none
                }
                return state.attachments.allReady ? .send(.submit) : .none
            case .actions(.sent(let request)):
                state.comments = [:]
                state.reply = ""
                return .merge(.send(.delegate(.handOff(request))), .run { [dismiss] _ in await dismiss() })
            case .actions:
                return .none
            }
        }
    }

    /// The text a card carries itself, such as a plan Claude Code holds, when it can be commented on.
    static func commentableText(of document: ViewDocument) -> String? {
        document.sections.lazy.flatMap(\.items).compactMap { item -> String? in
            if case .markdown(let markdown) = item, markdown.commentable { markdown.text } else { nil }
        }.first
    }

    /// The screen never stays on its placeholder: past the same limit as a card in the queue it
    /// says it could not load and offers to try again.
    private func loadDeadline(_ state: State) -> Effect<Action> {
        guard state.screen.document == nil else { return .none }
        return .run { [clock] send in
            try await clock.sleep(for: TriageFeature.loadDeadline)
            await send(.loadTimedOut)
        }
        .cancellable(id: CancelID.loadDeadline, cancelInFlight: true)
    }

    private func fetchContent(of document: ViewDocument, state: inout State) -> Effect<Action> {
        guard let reference = document.sections.lazy.flatMap(\.items).compactMap({ item -> ContentRef? in
            if case .contentRef(let reference) = item, reference.kind == .document { reference } else { nil }
        }).first, reference.digest != state.contentDigest || state.contentFailed else { return .none }
        state.contentDigest = reference.digest
        state.content = nil
        state.contentFailed = false
        return .run { [viewsClient] send in
            do { await send(.contentLoaded(digest: reference.digest, try await viewsClient.fetchContent(reference))) }
            catch is CancellationError { return }
            catch { await send(.contentFailed) }
        }
    }

    private func fetchDiff(of document: ViewDocument, state: inout State) -> Effect<Action> {
        guard let reference = document.sections.lazy.flatMap(\.items).compactMap({ item -> ContentRef? in
            if case .contentRef(let reference) = item, reference.kind == .diff { reference } else { nil }
        }).first, reference.digest != state.diffDigest else { return .none }
        state.diffDigest = reference.digest
        state.diff = nil
        state.diffTruncated = false
        state.diffFailed = false
        return .run { [viewsClient] send in
            do {
                let fetched = try await viewsClient.fetchDiff(reference)
                await send(.diffLoaded(digest: reference.digest, UnifiedDiff.parse(fetched.text), truncated: fetched.truncated, marks: fetched.marks))
            }
            catch is CancellationError { return }
            catch { await send(.diffFailed(digest: reference.digest)) }
        }
    }
}
