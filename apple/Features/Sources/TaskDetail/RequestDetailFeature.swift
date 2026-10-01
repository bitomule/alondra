import AlondraContract
import ComposableArchitecture
import Foundation

/// One request: its state, tasks and text, and withdrawing it.
@Reducer
public struct RequestDetailFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var actions = ActionFeature.State.ios()
        public init(key: ViewKey) { screen = ScreenFeature.State(key: key) }

        public var detail: RequestDetail? { screen.document.map(RequestDetail.init) }
    }

    public enum Action {
        case task
        case open(AlondraContract.Link)
        case withdraw
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case delegate(ScreenDelegate)
    }

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
            case .open(let link):
                return ScreenDelegate.opening(link).map { .send(.delegate($0)) } ?? .none
            case .withdraw:
                guard let withdraw = state.detail?.withdraw else { return .none }
                return .concatenate(.send(.actions(.present(withdraw))), .send(.actions(.submit)))
            case .actions(.delegate(let outcome)):
                return .concatenate(.send(.actions(.dismiss)), .send(.delegate(ScreenDelegate(outcome))))
            case .screen, .actions, .delegate:
                return .none
            }
        }
    }
}
