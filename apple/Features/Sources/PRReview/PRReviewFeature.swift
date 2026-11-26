import AlondraContract
import AlondraDesign
import AlondraFeed
import ComposableArchitecture
import Foundation

/// One pull request: its description and status, and the ways to act on it (merge by method,
/// resolve conflicts, ask, request changes). Questions and reports the agent attached to the PR
/// are answered here too, through the same action sheet.
@Reducer
public struct PRReviewFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var actions: ActionFeature.State

        public init(key: ViewKey) {
            screen = ScreenFeature.State(key: key)
            actions = .ios()
            actions.handsOff = true
        }

        public var review: PRReview? { screen.document.map(PRReview.init(document:)) }

        /// The sheet shows for answers that take text or a choice; a merge or a conflict hand-off
        /// is one tap and only ever asks for confirmation.
        public var isComposing: Bool { actions.action?.needsSheet == true }
    }

    public enum Action {
        case task
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case perform(WireAction, optionID: String? = nil)
        case dismissSheet
        case follow(AlondraContract.Link)
        case delegate(ScreenDelegate)
    }

    @Dependency(\.dismiss) var dismiss

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .screen(.loaded(let document)):
                state.actions.dataEpoch = document.dataEpoch
                return .none
            case .screen:
                return .none
            case .perform(let wire, let optionID):
                guard wire.enabled else { return .none }
                var effects: [Effect<Action>] = [.send(.actions(.present(wire)))]
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
            case .delegate:
                return .none
            }
        }
    }
}

private extension WireAction {
    var needsSheet: Bool {
        if case .text = input { return true }
        return presentation == .options
    }
}
