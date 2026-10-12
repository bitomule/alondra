import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// One node: who it is, what it is doing, what it needs, what it said last,
/// a composer to write to it, and the actions the daemon offers (close, retry, stop).
@Reducer
public struct NodeDetailFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var composer: ActionFeature.State
        public var actions: ActionFeature.State
        public var draft = ""
        public var optimistic = Optimistic()

        public init(key: ViewKey) {
            screen = ScreenFeature.State(key: key)
            composer = .ios()
            actions = .ios()
        }

        public var detail: NodeDetail? { screen.document.map { NodeDetail(document: $0).hiding(optimistic) } }

        public var canSend: Bool {
            composer.action != nil
                && (!draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !composer.attachmentPicker.entries.isEmpty)
        }
    }

    public enum Action {
        case task
        case screen(ScreenFeature.Action)
        case composer(ActionFeature.Action)
        case actions(ActionFeature.Action)
        case draftChanged(String)
        case send
        case perform(WireAction)
        case follow(AlondraContract.Link)
        case delegate(ScreenDelegate)
    }

    @Dependency(\.dismiss) var dismiss

    /// A message action keeps its subject when only the revision after `@` moves on.
    private static func subject(of action: WireAction?) -> Substring? {
        action?.id?.split(separator: "@").first
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.composer, action: \.composer) { ActionFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .screen(.loaded(let document)):
                state.optimistic.reconcile(document.version, dataEpoch: document.dataEpoch)
                state.composer.dataEpoch = document.dataEpoch
                state.actions.dataEpoch = document.dataEpoch
                guard let message = NodeDetail(document: document).message else { return .none }
                if Self.subject(of: state.composer.action) != Self.subject(of: message) {
                    return .send(.composer(.present(message)))
                }
                if state.composer.action?.id != message.id, !state.composer.inFlight {
                    state.composer.action = message
                }
                return .none
            case .screen:
                return .none
            case .draftChanged(let text):
                state.draft = text
                return .none
            case .send:
                guard state.canSend else { return .none }
                return .concatenate(.send(.composer(.textChanged(state.draft))), .send(.composer(.submit)))
            case .composer(.sent(let request)):
                guard request.dataEpoch == state.composer.dataEpoch else { return .none }
                state.draft = ""
                guard let message = state.composer.action else { return .none }
                return .send(.composer(.present(message)))
            case .composer(.settled(let request, applied: false)):
                guard request.dataEpoch == state.composer.dataEpoch else { return .none }
                if state.draft.isEmpty { state.draft = request.input.text ?? "" }
                return .none
            case .composer(.delegate(let delegate)), .actions(.delegate(let delegate)):
                return .send(.delegate(ScreenDelegate(delegate)))
            case .composer:
                return .none
            case .perform(let wire):
                state.actions.handsOff = wire.command == "close"
                return .concatenate(.send(.actions(.present(wire))), .send(.actions(.submit)))
            case .actions(.sent(let request)) where state.actions.handsOff:
                guard request.dataEpoch == state.actions.dataEpoch else { return .none }
                return .merge(.send(.delegate(.handOff(request))), .run { _ in await dismiss() })
            case .actions(.sent(let request)):
                state.optimistic.begin(request.actionId, request, dataEpoch: state.screen.document?.dataEpoch)
                return .none
            case .actions(.settled(let request, let applied)):
                state.optimistic.settle(request, applied: applied, shown: state.screen.document?.version)
                return .none
            case .actions:
                return .none
            case .follow(let link):
                guard let open = ScreenDelegate.opening(link) else { return .none }
                return .send(.delegate(open))
            case .delegate:
                return .none
            }
        }
    }
}
