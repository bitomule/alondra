import AlondraContract
import ComposableArchitecture
import Foundation

/// One task: its state, what its node is doing and needs, what it waits for from you, its request,
/// pull requests, dependencies, documents, options and history, a composer to write to its node,
/// with interrupt, relaunch, cancel, reopen, pause, resume and its priority. A tapped
/// action leaves the bar at once and returns if the daemon does not apply it; a chosen priority
/// shows at once.
@Reducer
public struct TaskDetailFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var actions = ActionFeature.State.ios()
        public var composer = ActionFeature.State.ios()
        public var message = ""
        public var optimistic = Optimistic()
        /// The option label chosen for each action sent with one, shown until the daemon answers.
        public var choices: [String: String] = [:]
        public init(key: ViewKey) { screen = ScreenFeature.State(key: key) }

        public var detail: TaskDetail? { screen.document.map(TaskDetail.init) }

        public var canSend: Bool {
            composer.action != nil
                && (!message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !composer.attachmentPicker.entries.isEmpty)
        }
    }

    public enum Action {
        case task
        case open(AlondraContract.Link)
        case perform(WireAction)
        case choose(WireAction, ActionOption)
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case composer(ActionFeature.Action)
        case messageChanged(String)
        case send
        case delegate(ScreenDelegate)
    }

    /// A message action keeps its subject when only the revision after `@` moves on.
    private static func subject(of action: WireAction?) -> Substring? {
        action?.id?.split(separator: "@").first
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Scope(state: \.composer, action: \.composer) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .screen(.loaded(let document)):
                state.actions.dataEpoch = document.dataEpoch
                state.composer.dataEpoch = document.dataEpoch
                state.optimistic.reconcile(document.version, dataEpoch: document.dataEpoch)
                guard let message = TaskDetail(document: document).message else {
                    if !state.composer.inFlight { state.composer.action = nil }
                    return .none
                }
                if Self.subject(of: state.composer.action) != Self.subject(of: message) {
                    return .send(.composer(.present(message)))
                }
                if state.composer.action?.id != message.id, !state.composer.inFlight {
                    state.composer.action = message
                }
                return .none
            case .messageChanged(let text):
                state.message = text
                return .none
            case .send:
                guard state.canSend else { return .none }
                return .concatenate(.send(.composer(.textChanged(state.message))), .send(.composer(.submit)))
            case .composer(.sent(let request)):
                guard request.dataEpoch == state.composer.dataEpoch else { return .none }
                state.message = ""
                guard let message = state.composer.action else { return .none }
                return .send(.composer(.present(message)))
            case .composer(.settled(let request, applied: false)):
                guard request.dataEpoch == state.composer.dataEpoch else { return .none }
                if state.message.isEmpty { state.message = request.input.text ?? "" }
                return .none
            case .composer(.delegate(let outcome)):
                return .send(.delegate(ScreenDelegate(outcome)))
            case .actions(.sent(let request)):
                state.optimistic.begin(request.actionId, request, dataEpoch: state.screen.document?.dataEpoch)
                return .none
            case .actions(.settled(let request, let applied)):
                state.optimistic.settle(request, applied: applied, shown: state.screen.document?.version)
                return .none
            case .open(let link):
                return ScreenDelegate.opening(link).map { .send(.delegate($0)) } ?? .none
            case .perform(let action):
                if let open = ScreenDelegate.opening(action.link) { return .send(.delegate(open)) }
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.submit)))
            case let .choose(action, option):
                if let id = action.id { state.choices[id] = option.label }
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.chooseOption(option.id))), .send(.actions(.submit)))
            case .actions(.delegate(let outcome)):
                return .concatenate(.send(.actions(.dismiss)), .send(.delegate(ScreenDelegate(outcome))))
            case .screen, .actions, .composer, .delegate:
                return .none
            }
        }
    }
}
