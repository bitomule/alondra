import AlondraAttachments
import AlondraContract
import ComposableArchitecture
import Foundation

/// One card of Waiting for You: it observes its own view, owns the reply draft and the picked
/// option, and decides what each control on the card does.
@Reducer
public struct TriageCardFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable, Identifiable {
        public let id: String
        public var screen: ScreenFeature.State
        public var actions: ActionFeature.State
        public var attachments = AttachmentFeature.State()
        public var replyDraft = ""
        public var contextRequest: ContextRequest?
        public var awaitingAttachments = false

        public init(id: String, dataEpoch: String = "") {
            self.id = id
            screen = ScreenFeature.State(key: .card(id))
            actions = .device(dataEpoch: dataEpoch)
        }

        public var card: Card? {
            screen.document?.sections.lazy.flatMap(\.items).compactMap { item -> Card? in
                if case .card(let card) = item { card } else { nil }
            }.first { $0.id == id }
        }

        public var allActions: [WireAction] {
            guard let card else { return [] }
            return card.actions + card.children.flatMap(\.actions)
        }

        public var selected: WireAction? {
            guard let action = actions.action, allActions.contains(where: { $0.id == action.id }) else { return nil }
            return action
        }

        public var stamp: String? { selected == nil ? nil : actions.stamp }

        /// The action the reply field sends: free text, or the option picked above it.
        public var replyAction: WireAction? {
            guard let card else { return nil }
            let preferred: [String] = switch card.icon {
            case .pullRequest, .document: ["changes", "reply", "ans", "ask_pr"]
            default: ["ans", "reply", "changes", "ask_pr"]
            }
            for verb in preferred {
                if let action = allActions.first(where: { Self.isReplyAction($0, verb: verb) }) { return action }
            }
            return nil
        }

        private static func isReplyAction(_ action: WireAction, verb: String) -> Bool {
            guard action.enabled, action.id?.hasPrefix(verb + ":") == true else { return false }
            if action.presentation == .options { return true }
            if case .text = action.input { return true }
            return false
        }
    }

    public struct ContextRequest: Equatable, Sendable {
        public let action: WireAction
        public var note = ""
    }

    public enum Action {
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case attachments(AttachmentFeature.Action)
        case replyChanged(String)
        case optionPicked(WireAction, optionID: String)
        case menuOptionPicked(WireAction, optionID: String)
        case replySent(WireAction)
        case actionTapped(WireAction)
        case contextNoteChanged(String)
        case opened(ViewKey)
        case contextConfirmed
        case contextCancelled
        case submit
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case open(ViewKey)
        }
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Scope(state: \.attachments, action: \.attachments) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .screen(.loaded(let document)):
                guard document.key == state.screen.key.rawValue else { return .none }
                state.actions.dataEpoch = document.dataEpoch
                return .none
            case .screen, .delegate:
                return .none
            case .opened(let key):
                return .send(.delegate(.open(key)))
            case .replyChanged(let text):
                state.replyDraft = text
                guard !text.isEmpty, let reply = state.replyAction, state.selected?.id == reply.id,
                      let optionID = state.actions.selectedOptionID,
                      let option = reply.options.first(where: { $0.id == optionID }),
                      case .none = option.input else { return .none }
                return .send(.actions(.present(reply)))
            case .optionPicked(let action, let optionID):
                return .concatenate(
                    state.selected?.id == action.id ? .none : .send(.actions(.present(action))),
                    .send(.actions(.chooseOption(optionID)))
                )
            case .menuOptionPicked(let action, let optionID):
                return .concatenate(
                    .send(.actions(.present(action))), .send(.actions(.chooseOption(optionID))), .send(.submit)
                )
            case .replySent(let action):
                return .concatenate(
                    state.selected?.id == action.id ? .none : .send(.actions(.present(action))),
                    state.replyDraft.isEmpty ? .none : .send(.actions(.textChanged(state.replyDraft))),
                    .send(.submit)
                )
            case .actionTapped(let action):
                return tapped(action, state: &state)
            case .contextNoteChanged(let note):
                state.contextRequest?.note = note
                return .none
            case .contextCancelled:
                state.contextRequest = nil
                return .none
            case .contextConfirmed:
                guard let request = state.contextRequest else { return .none }
                state.contextRequest = nil
                return .concatenate(
                    .send(.actions(.present(request.action))),
                    .send(.actions(.textChanged(request.note))),
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
            case .actions(.response(let response)):
                guard response.outcome == .applied else { return .none }
                state.replyDraft = ""
                return .send(.attachments(.reset))
            case .actions:
                return .none
            }
        }
    }

    private func tapped(_ action: WireAction, state: inout State) -> Effect<Action> {
        let verb = action.id?.split(separator: ":", maxSplits: 1).first.map(String.init) ?? ""
        switch verb {
        case "ctx":
            state.contextRequest = ContextRequest(action: action)
            return .none
        case "ask_pr":
            return .concatenate(
                .send(.actions(.present(action))),
                .send(.actions(.textChanged(state.replyDraft))),
                .send(.submit)
            )
        default:
            break
        }
        if action.comments != nil {
            if case .view(let key) = action.link { return .send(.opened(ViewKey(rawValue: key))) }
            return .send(.opened(.card(state.id)))
        }
        if let link = action.link {
            if case .view(let key) = link { return .send(.opened(ViewKey(rawValue: key))) }
            return .none
        }
        var effects: [Effect<Action>] = [.send(.actions(.present(action)))]
        if case .none = action.input, action.presentation == .button { effects.append(.send(.submit)) }
        return .concatenate(effects)
    }
}
