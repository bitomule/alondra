import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// The Fleet tab: the orchestrator, the fleet's pace, every node grouped by project, a search the
/// daemon answers, and the actions the daemon offers: interrupting, pausing, resuming or closing a
/// node, and changing how many work at once. A closed node leaves the list at once and returns if
/// the daemon does not close it.
@Reducer
public struct FleetFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen = ScreenFeature.State(key: .fleet)
        public var search: ScreenFeature.State?
        public var query = ""
        public var actions: ActionFeature.State
        public var optimistic = Optimistic()

        public init() {
            actions = .ios()
        }

        public var isSearching: Bool { !query.trimmingCharacters(in: .whitespaces).isEmpty }
        public var overview: FleetOverview? { screen.document.map { FleetOverview(document: $0).hiding(optimistic) } }
        public var results: FleetOverview? { search?.document.map { FleetOverview(document: $0).hiding(optimistic) } }
    }

    public enum Action {
        case task
        case refresh
        case screen(ScreenFeature.Action)
        case search(ScreenFeature.Action)
        case queryChanged(String)
        case searchStarted(String)
        case perform(WireAction)
        case actions(ActionFeature.Action)
        case follow(AlondraContract.Link)
        /// A node's own screen closed it and left; the app delivers the close.
        case handedOff(ActionRequest)
        case handOffSettled(ActionRequest, applied: Bool)
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case screen(ScreenDelegate)
            case closeRefused(String)
        }
    }

    @Dependency(\.continuousClock) var clock
    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID { case debounce }
    static let searchDelay = Duration.milliseconds(250)

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .refresh:
                let search = state.search?.key
                return .run { [viewsClient] send in
                    do {
                        if let document = try await viewsClient.document(.fleet) {
                            await send(.screen(.loaded(document)))
                        }
                        if let search, let document = try await viewsClient.document(search) {
                            await send(.search(.loaded(document)))
                        }
                    } catch is CancellationError {
                        return
                    } catch {
                        await send(.delegate(.screen(.failed(String(localized: "Couldn’t load this from your Mac. Try again.")))))
                    }
                }
            case .screen(.loaded(let document)):
                state.actions.dataEpoch = document.dataEpoch
                state.optimistic.reconcile(document.version)
                return .none
            case .screen, .search:
                return .none
            case .queryChanged(let query):
                state.query = query
                guard state.isSearching else {
                    state.search = nil
                    return .cancel(id: CancelID.debounce)
                }
                return .run { [clock] send in
                    try await clock.sleep(for: Self.searchDelay)
                    await send(.searchStarted(query))
                }
                .cancellable(id: CancelID.debounce, cancelInFlight: true)
            case .searchStarted(let query):
                guard query == state.query, state.isSearching else { return .none }
                state.search = ScreenFeature.State(key: .search(query.trimmingCharacters(in: .whitespaces)))
                return .send(.search(.task))
            case .perform(let wire):
                return .concatenate(.send(.actions(.present(wire))), .send(.actions(.submit)))
            case .actions(.response(let response)) where response.outcome == .rejected && state.actions.action?.id?.hasPrefix("close:") == true:
                return .send(.delegate(.closeRefused(response.message ?? String(localized: "Couldn’t close the node"))))
            case .actions(.sent(let request)), .handedOff(let request):
                state.optimistic.begin(request.actionId, request)
                return .none
            case .actions(.settled(let request, let applied)), .handOffSettled(let request, let applied):
                state.optimistic.settle(request, applied: applied, shown: state.screen.document?.version)
                return .none
            case .actions(.delegate(let delegate)):
                return .send(.delegate(.screen(ScreenDelegate(delegate))))
            case .actions:
                return .none
            case .follow(let link):
                guard let open = ScreenDelegate.opening(link) else { return .none }
                return .send(.delegate(.screen(open)))
            case .delegate:
                return .none
            }
        }
        .ifLet(\.search, action: \.search) { ScreenFeature() }
    }
}
