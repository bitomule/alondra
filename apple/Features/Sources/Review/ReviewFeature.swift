import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeed
import ComposableArchitecture
import Foundation

/// Reading a document, a delivery or any card in full, with a comment on any block, and the
/// actions the server allows for it. The comments, the general note and the attachments are drafts
/// until one of those actions is sent.
@Reducer
public struct ReviewFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var actions = ActionFeature.State.device()
        public var attachments = AttachmentFeature.State()
        public var content: String?
        public var contentDigest: String?
        public var contentFailed = false
        public var comments: [String: ActionComment] = [:]
        public var general = ""
        public var composing: Composing?
        public var preview: AttachmentPreview?
        public var awaitingAttachments = false

        public init(key: ViewKey) { screen = ScreenFeature.State(key: key) }

        public var subject: Subject? { Subject(document: screen.document, key: screen.key) }
        public var contentPending: Bool { contentDigest != nil && content == nil && !contentFailed }
        public var markdown: String { content ?? subject?.body ?? "" }
        public var blocks: [MarkdownBlock] { MarkdownParser.blocks(from: markdown) }
        public var feedback: [ActionComment] { blocks.compactMap { comments[$0.draftKey] } }
        public var canComment: Bool { subject?.actions.contains { $0.comments?.kind == .blocks } == true }
        public var acceptsFeedback: Bool {
            canComment || subject?.actions.contains { $0.id?.hasPrefix("reply:") == true } == true
        }
        public var hasFeedback: Bool {
            !comments.isEmpty || !general.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                || !attachments.entries.isEmpty
        }

        public func isSendable(_ action: WireAction) -> Bool {
            guard action.enabled else { return false }
            let needsFeedback = action.comments != nil || action.id?.hasPrefix("reply:") == true
            return !needsFeedback || hasFeedback
        }
    }

    /// What the review shows, whether the server sent a card or a document revision.
    public struct Subject: Equatable, Sendable {
        public let icon: Icon
        public let title: String
        public let nodeName: String?
        public let createdAt: String?
        public let body: String?
        public let facts: Facts?
        public let checks: Checks?
        public let attachments: [Attachment]
        public let children: [Card]
        public let actions: [WireAction]

        init?(document: ViewDocument?, key: ViewKey) {
            guard let document else { return nil }
            let items = document.sections.flatMap(\.items)
            let bar = items.flatMap { item -> [WireAction] in
                if case .actions(let bar) = item { bar.actions } else { [] }
            }
            let card: Card? = key.rawValue.hasPrefix("card:")
                ? items.lazy.compactMap { item -> Card? in if case .card(let card) = item { card } else { nil } }.first
                : nil
            var seen = Set<String>()
            actions = ((card?.actions ?? []) + bar).filter { action in
                guard let id = action.id, !id.isEmpty else { return true }
                return seen.insert(id).inserted
            }
            icon = card?.icon ?? .document
            title = card?.title ?? document.title ?? ""
            nodeName = card?.context.first { $0.icon == .node }?.text
            createdAt = card?.timestamps.first?.at
            body = card?.body?.text
            let documentFacts = items.lazy.compactMap { item -> Facts? in
                if case .facts(let facts) = item { facts } else { nil }
            }.first
            facts = card?.facts ?? documentFacts
            checks = card?.checks
            attachments = card?.attachments ?? []
            children = card?.children ?? []
        }

        /// The actions the bar offers, in the order they read: change requests first, then the
        /// approval. A delivery never offers "Mark as Read"; the question's options sit in the page.
        public var barActions: [WireAction] {
            let review = actions.filter { Self.verb($0) == "changes" || Self.verb($0) == "accept" }
            let candidates = review.isEmpty ? actions : review
            return candidates
                .filter { $0.presentation != .options }
                .filter { !(icon == .delivery && Self.verb($0) == "read") }
                .sorted { Self.rank($0) < Self.rank($1) }
        }

        /// A question's options in the page; a plan approved from the bar does not repeat them.
        public var options: WireAction? {
            guard [Icon.question, .action, .document].contains(icon),
                  !actions.contains(where: { Self.verb($0) == "accept" }) else { return nil }
            return actions.first { $0.presentation == .options && $0.enabled }
        }

        public func isRescue(_ action: WireAction) -> Bool {
            icon == .delivery && Self.verb(action) == "changes" && action.comments != nil
        }

        public static func verb(_ action: WireAction) -> String {
            action.id?.split(separator: ":", maxSplits: 1).first.map(String.init) ?? ""
        }

        private static func rank(_ action: WireAction) -> Int {
            switch verb(action) {
            case "changes": 0
            case "accept": 1
            default: 2
            }
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
        case blockTapped(anchor: String, quote: String)
        case composingChanged(String)
        case commentSaved
        case commentDeleted
        case commentCancelled
        case generalChanged(String)
        case actionTapped(WireAction)
        case optionPicked(WireAction, optionID: String)
        case attachmentTapped(Attachment)
        case attachmentLoaded(Attachment, Data)
        case previewClosed
        case opened(ViewKey)
        case submit
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case open(ViewKey)
        }
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.dismiss) var dismiss

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Scope(state: \.attachments, action: \.attachments) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .screen(.loaded(let document)):
                guard document.key == state.screen.key.rawValue else { return .none }
                state.actions.dataEpoch = document.dataEpoch
                return fetchContent(of: document, state: &state)
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
            case .generalChanged(let text):
                state.general = text
                return .none
            case .actionTapped(let action):
                if case .view(let key) = action.link { return .send(.opened(ViewKey(rawValue: key))) }
                guard state.isSendable(action) else { return .none }
                return .concatenate(
                    .send(.actions(.present(action))),
                    .send(.actions(.commentsChanged(state.feedback))),
                    .send(.actions(.textChanged(state.general))),
                    .send(.submit)
                )
            case .optionPicked(let action, let optionID):
                return .concatenate(
                    .send(.actions(.present(action))), .send(.actions(.chooseOption(optionID))), .send(.submit)
                )
            case .attachmentTapped(let attachment):
                return .run { [viewsClient] send in
                    guard let data = try? await viewsClient.fetchAttachment(attachment) else {
                        await send(.actions(.failed(String(localized: "Couldn’t load this from your Mac. Try again."))))
                        return
                    }
                    await send(.attachmentLoaded(attachment, data))
                }
            case .attachmentLoaded(let attachment, let data):
                state.preview = AttachmentPreview(attachment: attachment, data: data)
                return .none
            case .previewClosed:
                state.preview = nil
                return .none
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
            case .actions(.response(let response)):
                guard response.outcome == .applied else { return .none }
                state.comments = [:]
                state.general = ""
                return .run { [dismiss] _ in await dismiss() }
            case .actions:
                return .none
            }
        }
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
}
