import AlondraContract
import ComposableArchitecture
import Foundation

/// One task: its state, what it waits for from you, its request, pull requests, dependencies,
/// documents, options and history, with relaunch, cancel, reopen, archive, pause, resume and its
/// priority. A tapped action leaves the bar at once and returns if the daemon does not apply it; a
/// chosen priority shows at once.
@Reducer
public struct TaskDetailFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var actions = ActionFeature.State.ios()
        public var optimistic = Optimistic()
        /// The option label chosen for each action sent with one, shown until the daemon answers.
        public var choices: [String: String] = [:]
        public init(key: ViewKey) { screen = ScreenFeature.State(key: key) }

        public var detail: TaskDetail? { screen.document.map(TaskDetail.init) }
    }

    public enum Action {
        case task
        case open(AlondraContract.Link)
        case perform(WireAction)
        case choose(WireAction, ActionOption)
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
                state.optimistic.reconcile(document.version)
                return .none
            case .actions(.sent(let request)):
                state.optimistic.begin(request.actionId, request)
                return .none
            case .actions(.settled(let request, let applied)):
                state.optimistic.settle(request, applied: applied, shown: state.screen.document?.version)
                return .none
            case .open(let link):
                return ScreenDelegate.opening(link).map { .send(.delegate($0)) } ?? .none
            case .perform(let action):
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.submit)))
            case let .choose(action, option):
                if let id = action.id { state.choices[id] = option.label }
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.chooseOption(option.id))), .send(.actions(.submit)))
            case .actions(.delegate(let outcome)):
                return .concatenate(.send(.actions(.dismiss)), .send(.delegate(ScreenDelegate(outcome))))
            case .screen, .actions, .delegate:
                return .none
            }
        }
    }
}
