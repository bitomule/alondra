import AlondraHomeFeature
import AlondraViews
import AlondraViewsDependency
import AlondraViewsUI
import ComposableArchitecture
import Foundation

/// The shipping workspace owns only server Views. The older domain presentation is kept in
/// its separate test target and is never started by an approved service scene.
@Reducer
public struct ViewsServiceRootFeature {
    public init() {}

    @ObservableState
    public struct State {
        public init() {}
        public enum Tab: Hashable { case inbox, tasks, fleet, newRequest, account }
        public var tab = Tab.inbox
        public var settingsPresented = false
        public var notice: ViewsWorkspaceNotice?
        public var macError: ViewsWorkspaceNotice?
        public var closeRefusal: String?
        public var home = HomeFeature.State()
        public var macPresentation = MacPresentationFeature.State()
        public var serverTasks = ViewsScreenFeature.State(key: .tasks, owner: "ios.tasks")
        public var serverFleet = ViewsScreenFeature.State(key: .fleet, owner: "ios.fleet")
        public var serverNewRequest = ViewsScreenFeature.State(key: .newRequest(), owner: "ios.new_request")
        public var serverTasksPath = StackState<ViewsScreenFeature.State>()
        public var serverFleetPath = StackState<ViewsScreenFeature.State>()
        public var serverNewRequestPath = StackState<ViewsScreenFeature.State>()
        public var serverReviewPath = StackState<ViewsScreenFeature.State>()
    }

    public enum Action {
        case appeared, disappeared
        case tabSelected(State.Tab), settingsPresented(Bool), focusInbox(String)
        case openServerView(String), openTasksView(ViewKey), openFleetView(ViewKey), openNewRequestView(ViewKey), openReviewView(ViewKey)
        case dismissNotice(UUID), dismissMacError(UUID), dismissCloseRefusal
        case home(HomeFeature.Action)
        case macPresentation(MacPresentationFeature.Action)
        case serverTasks(ViewsScreenFeature.Action)
        case serverFleet(ViewsScreenFeature.Action)
        case serverNewRequest(ViewsScreenFeature.Action)
        case serverTasksPath(StackActionOf<ViewsScreenFeature>)
        case serverFleetPath(StackActionOf<ViewsScreenFeature>)
        case serverNewRequestPath(StackActionOf<ViewsScreenFeature>)
        case serverReviewPath(StackActionOf<ViewsScreenFeature>)
        case viewsFeed(FeedEvent)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    private enum CancelID { case feed, notice, macError }

    public var body: some ReducerOf<Self> {
        Scope(state: \.home, action: \.home) { HomeFeature() }
        Scope(state: \.macPresentation, action: \.macPresentation) { MacPresentationFeature() }
        Scope(state: \.serverTasks, action: \.serverTasks) { ViewsScreenFeature() }
        Scope(state: \.serverFleet, action: \.serverFleet) { ViewsScreenFeature() }
        Scope(state: \.serverNewRequest, action: \.serverNewRequest) { ViewsScreenFeature() }
        Reduce { state, action in
            if case .dismissNotice(let id) = action {
                guard state.notice?.id == id else { return .none }
                state.notice = nil
                return .cancel(id: CancelID.notice)
            }
            if case .dismissMacError(let id) = action {
                guard state.macError?.id == id else { return .none }
                state.macError = nil
                return .cancel(id: CancelID.macError)
            }
            if case .dismissCloseRefusal = action { state.closeRefusal = nil; return .none }
            if case .serverFleet(.action(.response(let response))) = action,
               state.serverFleet.action.action?.id?.hasPrefix("close:") == true,
               response.outcome == .rejected {
                state.closeRefusal = response.message ?? String(localized: "Couldn’t close the node")
                return .none
            }
            if let event = noticeEvent(action) {
                let notice = ViewsWorkspaceNotice(event: event)
                state.notice = notice
                return .run { send in
                    try await clock.sleep(for: .seconds(notice.failure ? 4 : 2.2))
                    await send(.dismissNotice(notice.id))
                }.cancellable(id: CancelID.notice, cancelInFlight: true)
            }
            switch action {
            case .appeared:
                return .run { [viewsClient] send in
                    for await event in await viewsClient.feed() { await send(.viewsFeed(event)) }
                }.cancellable(id: CancelID.feed, cancelInFlight: true)
            case .disappeared:
                return .merge(.send(.home(.disappeared)), .cancel(id: CancelID.feed))
            case .tabSelected(let tab): state.tab = tab; return .none
            case .settingsPresented(let value): state.settingsPresented = value; return .none
            case .focusInbox(let id):
                state.tab = .inbox
                let publicID = String(id.split(separator: ":").last ?? Substring(id))
                return .send(.home(.focus(publicID)))
            case .openServerView(let key):
                state.serverReviewPath.removeAll()
                append(ViewKey(rawValue: key), to: &state.serverReviewPath, owner: "ios.detail")
                return .none
            case .openTasksView(let key):
                append(key, to: &state.serverTasksPath, owner: "ios.tasks")
                return .none
            case .openFleetView(let key):
                append(key, to: &state.serverFleetPath, owner: "ios.fleet")
                return .none
            case .openNewRequestView(let key):
                append(key, to: &state.serverNewRequestPath, owner: "ios.new_request")
                return .none
            case .openReviewView(let key):
                append(key, to: &state.serverReviewPath, owner: "ios.detail")
                return .none
            case .viewsFeed(.stale(let keys)):
                return .merge(.send(.home(.stale(keys))), .send(.macPresentation(.viewsStale(keys))),
                    .send(.serverTasks(.stale(keys))), .send(.serverFleet(.stale(keys))),
                    .send(.serverNewRequest(.stale(keys))), pathEffects(state, action: .stale(keys)))
            case .viewsFeed(.reset):
                return .merge(.send(.home(.reset)), .send(.macPresentation(.viewsReset)),
                    .send(.serverTasks(.reset)), .send(.serverFleet(.reset)),
                    .send(.serverNewRequest(.reset)), pathEffects(state, action: .reset))
            case .viewsFeed(.snapshot(let snapshot)):
                if snapshot.reset { return .send(.viewsFeed(.reset)) }
                if let status = snapshot.status { return .send(.home(.feedStatus(status))) }
                return .none
            case .macPresentation(.viewFailed(_, let message)):
                let notice = ViewsWorkspaceNotice(event: .failure(message))
                state.macError = notice
                return .run { send in
                    try await clock.sleep(for: .seconds(4))
                    await send(.dismissMacError(notice.id))
                }.cancellable(id: CancelID.macError, cancelInFlight: true)
            case .home, .macPresentation, .serverTasks, .serverFleet, .serverNewRequest,
                 .serverTasksPath, .serverFleetPath, .serverNewRequestPath, .serverReviewPath,
                 .dismissNotice, .dismissMacError, .dismissCloseRefusal:
                return .none
            }
        }
        .forEach(\.serverTasksPath, action: \.serverTasksPath) { ViewsScreenFeature() }
        .forEach(\.serverFleetPath, action: \.serverFleetPath) { ViewsScreenFeature() }
        .forEach(\.serverNewRequestPath, action: \.serverNewRequestPath) { ViewsScreenFeature() }
        .forEach(\.serverReviewPath, action: \.serverReviewPath) { ViewsScreenFeature() }
    }

    private func append(_ key: ViewKey, to path: inout StackState<ViewsScreenFeature.State>, owner: String) {
        guard path.last?.key != key else { return }
        path.append(ViewsScreenFeature.State(key: key, owner: "\(owner).\(path.count)"))
    }

    private func pathEffects(_ state: State, action: ViewsScreenFeature.Action) -> Effect<Action> {
        let tasks: [Effect<Action>] = state.serverTasksPath.ids.map {
            .send(.serverTasksPath(.element(id: $0, action: action)))
        }
        let fleet: [Effect<Action>] = state.serverFleetPath.ids.map {
            .send(.serverFleetPath(.element(id: $0, action: action)))
        }
        let newRequest: [Effect<Action>] = state.serverNewRequestPath.ids.map {
            .send(.serverNewRequestPath(.element(id: $0, action: action)))
        }
        let review: [Effect<Action>] = state.serverReviewPath.ids.map {
            .send(.serverReviewPath(.element(id: $0, action: action)))
        }
        let effects = tasks + fleet + newRequest + review
        return .merge(effects)
    }

    private func noticeEvent(_ action: Action) -> ViewsWorkspaceNotice.Event? {
        let child: ActionFeature.Action
        switch action {
        case .home(.actions(let value)),
             .serverTasks(.action(let value)), .serverFleet(.action(let value)),
             .serverNewRequest(.action(let value)),
             .serverTasksPath(.element(id: _, action: .action(let value))),
             .serverFleetPath(.element(id: _, action: .action(let value))),
             .serverNewRequestPath(.element(id: _, action: .action(let value))),
             .serverReviewPath(.element(id: _, action: .action(let value))),
             .macPresentation(.action(let value)):
            child = value
        default: return nil
        }
        switch child {
        case .response(let response):
            switch response.outcome {
            case .applied: return .actionCompleted(response.message ?? String(localized: "Action completed"))
            case .rejected, .noLongerAvailable: return .failure(response.message ?? String(localized: "Action failed"))
            case .changed: return nil
            }
        case .failed(let message): return .failure(message)
        default: return nil
        }
    }
}

public struct ViewsWorkspaceNotice: Equatable, Sendable, Identifiable {
    public enum Event: Equatable, Sendable {
        case actionCompleted(String), failure(String)
    }
    public let id = UUID()
    public let event: Event
    public var failure: Bool {
        if case .failure = event { return true }
        return false
    }
    public init(event: Event) { self.event = event }
}
