import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// The scheduled tasks: open one, add one, run one now, pause or resume it, delete it.
@Reducer
public struct SchedulesFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen = ScreenFeature.State(key: .schedules)
        public var actions = ActionFeature.State.ios()
        public var optimistic = Optimistic()
        public init() {}

        public var list: ScheduleList? { screen.document.map { ScheduleList(document: $0).hiding(optimistic) } }
    }

    public enum Action {
        case task
        case refresh
        case open(AlondraContract.Link)
        case add
        case perform(WireAction)
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case delegate(ScreenDelegate)
    }

    @Dependency(\.viewsClient) var viewsClient

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .refresh:
                return .run { [viewsClient] send in
                    do {
                        guard let document = try await viewsClient.document(.schedules) else { return }
                        await send(.screen(.loaded(document)))
                    } catch is CancellationError {
                        return
                    } catch {
                        await send(.delegate(.failed(String(localized: "Couldn’t load this from your Mac. Try again."))))
                    }
                }
            case .screen(.loaded(let document)):
                state.actions.dataEpoch = document.dataEpoch
                state.optimistic.reconcile(document.version, dataEpoch: document.dataEpoch)
                return .none
            case .actions(.sent(let request)):
                state.optimistic.begin(request.actionId, request, dataEpoch: state.screen.document?.dataEpoch)
                return .none
            case .actions(.settled(let request, let applied)):
                state.optimistic.settle(request, applied: applied, shown: state.screen.document?.version)
                return .none
            case .open(let link):
                return ScreenDelegate.opening(link).map { .send(.delegate($0)) } ?? .none
            case .add:
                return state.list?.add.map { .send(.delegate($0)) } ?? .none
            case .perform(let action):
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.submit)))
            case .actions(.delegate(let outcome)):
                return .concatenate(.send(.actions(.dismiss)), .send(.delegate(ScreenDelegate(outcome))))
            case .screen, .actions, .delegate:
                return .none
            }
        }
    }
}
