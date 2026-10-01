import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// The iPhone's root: four tabs, each with its own stack of pushed screens, the settings sheet, the
/// toast, and where a tapped notification lands. It subscribes to the feed for the app-level status
/// and for the screens that still refresh through the root (`legacy`); the rebuilt screens observe
/// their own key.
@Reducer
public struct AppFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public init() {}
        public enum Tab: Hashable, CaseIterable, Sendable { case inbox, tasks, fleet, newRequest }
        public var tab = Tab.inbox
        public var settingsPresented = false
        public var notice: WorkspaceNotice?
        public var closeRefusal: String?
        public var triage = TriageFeature.State()
        public var tasks = ViewsScreenFeature.State(key: .tasks, owner: "ios.tasks")
        public var fleet = ViewsScreenFeature.State(key: .fleet, owner: "ios.fleet")
        public var newRequest = ViewsScreenFeature.State(key: .newRequest(), owner: "ios.new_request")
        public var inboxPath = StackState<DetailRoute.State>()
        public var tasksPath = StackState<DetailRoute.State>()
        public var fleetPath = StackState<DetailRoute.State>()
        public var newRequestPath = StackState<DetailRoute.State>()
    }

    public enum Action {
        case appeared, disappeared
        case tabSelected(State.Tab)
        case settingsPresented(Bool)
        /// A card was tapped in Waiting for You: its screen replaces whatever was pushed there.
        case openFromTriage(ViewKey)
        case open(State.Tab, ViewKey)
        /// A notification was tapped: show that card at the front of Waiting for You.
        case notificationOpened(cardID: String)
        case dismissNotice(UUID)
        case dismissCloseRefusal
        case triage(TriageFeature.Action)
        case tasks(ViewsScreenFeature.Action)
        case fleet(ViewsScreenFeature.Action)
        case newRequest(ViewsScreenFeature.Action)
        case inboxPath(StackActionOf<DetailRoute>)
        case tasksPath(StackActionOf<DetailRoute>)
        case fleetPath(StackActionOf<DetailRoute>)
        case newRequestPath(StackActionOf<DetailRoute>)
        case viewsFeed(FeedEvent)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    private enum CancelID { case feed, notice }

    public var body: some ReducerOf<Self> {
        Scope(state: \.triage, action: \.triage) { TriageFeature() }
        Scope(state: \.tasks, action: \.tasks) { ViewsScreenFeature() }
        Scope(state: \.fleet, action: \.fleet) { ViewsScreenFeature() }
        Scope(state: \.newRequest, action: \.newRequest) { ViewsScreenFeature() }
        Reduce { state, action in
            if let toast = Self.toast(for: action) {
                let notice = WorkspaceNotice(event: toast)
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
                return .merge(.send(.triage(.disappeared)), .cancel(id: CancelID.feed))
            case .tabSelected(let tab):
                state.tab = tab
                return .none
            case .settingsPresented(let value):
                state.settingsPresented = value
                return .none
            case .dismissNotice(let id):
                guard state.notice?.id == id else { return .none }
                state.notice = nil
                return .cancel(id: CancelID.notice)
            case .dismissCloseRefusal:
                state.closeRefusal = nil
                return .none
            case .notificationOpened(let cardID):
                state.tab = .inbox
                state.settingsPresented = false
                state.inboxPath.removeAll()
                let publicID = String(cardID.split(separator: ":").last ?? Substring(cardID))
                return .send(.triage(.focus(publicID)))
            case .triage(.delegate(.open(let key))):
                return .send(.openFromTriage(key))
            case .openFromTriage(let key):
                state.inboxPath.removeAll()
                Self.push(key, onto: &state.inboxPath, owner: "ios.detail")
                return .none
            case .open(let tab, let key):
                switch tab {
                case .inbox: Self.push(key, onto: &state.inboxPath, owner: "ios.detail")
                case .tasks: Self.push(key, onto: &state.tasksPath, owner: "ios.tasks")
                case .fleet: Self.push(key, onto: &state.fleetPath, owner: "ios.fleet")
                case .newRequest: Self.push(key, onto: &state.newRequestPath, owner: "ios.new_request")
                }
                return .none
            case .fleet(.action(.response(let response))):
                guard state.fleet.action.action?.id?.hasPrefix("close:") == true,
                      response.outcome == .rejected else { return .none }
                state.closeRefusal = response.message ?? String(localized: "Couldn’t close the node")
                return .none
            case .viewsFeed(.stale(let keys)):
                return .merge(
                    .send(.tasks(.stale(keys))), .send(.fleet(.stale(keys))), .send(.newRequest(.stale(keys))),
                    Self.refreshPaths(state, .stale(keys)))
            case .viewsFeed(.reset):
                return .merge(
                    .send(.tasks(.reset)), .send(.fleet(.reset)), .send(.newRequest(.reset)),
                    Self.refreshPaths(state, .reset))
            case .viewsFeed(.snapshot(let snapshot)):
                if snapshot.reset { return .send(.viewsFeed(.reset)) }
                if let status = snapshot.status { return .send(.triage(.feedStatus(status))) }
                return .none
            case .inboxPath(.element(id: _, action: .review(.delegate(.open(let key))))):
                Self.push(key, onto: &state.inboxPath, owner: "ios.detail")
                return .none
            case .tasksPath(.element(id: _, action: .review(.delegate(.open(let key))))):
                Self.push(key, onto: &state.tasksPath, owner: "ios.tasks")
                return .none
            case .fleetPath(.element(id: _, action: .review(.delegate(.open(let key))))):
                Self.push(key, onto: &state.fleetPath, owner: "ios.fleet")
                return .none
            case .newRequestPath(.element(id: _, action: .review(.delegate(.open(let key))))):
                Self.push(key, onto: &state.newRequestPath, owner: "ios.new_request")
                return .none
            case .triage, .tasks, .fleet, .newRequest, .inboxPath, .tasksPath, .fleetPath, .newRequestPath:
                return .none
            }
        }
        .forEach(\.inboxPath, action: \.inboxPath)
        .forEach(\.tasksPath, action: \.tasksPath)
        .forEach(\.fleetPath, action: \.fleetPath)
        .forEach(\.newRequestPath, action: \.newRequestPath)
    }

    /// A key outside the route table opens nothing, and a screen already on top is not pushed twice.
    private static func push(_ key: ViewKey, onto path: inout StackState<DetailRoute.State>, owner: String) {
        guard path.last?.key != key,
              let route = DetailRoute.State(key: key, owner: "\(owner).\(path.count)") else { return }
        path.append(route)
    }

    /// The screens still on the generic renderer refresh through the root; every rebuilt screen
    /// observes its own key.
    private static func refreshPaths(_ state: State, _ action: ViewsScreenFeature.Action) -> Effect<Action> {
        func legacyIDs(_ path: StackState<DetailRoute.State>) -> [StackElementID] {
            path.ids.filter { if case .legacy = path[id: $0] { true } else { false } }
        }
        return .merge(
            legacyIDs(state.inboxPath).map { .send(.inboxPath(.element(id: $0, action: .legacy(action)))) }
            + legacyIDs(state.tasksPath).map { .send(.tasksPath(.element(id: $0, action: .legacy(action)))) }
            + legacyIDs(state.fleetPath).map { .send(.fleetPath(.element(id: $0, action: .legacy(action)))) }
            + legacyIDs(state.newRequestPath).map { .send(.newRequestPath(.element(id: $0, action: .legacy(action)))) }
        )
    }

    private static func toast(for action: Action) -> WorkspaceNotice.Event? {
        let delegate: ActionFeature.Action.Delegate
        switch action {
        case .triage(.cards(.element(id: _, action: .actions(.delegate(let value))))),
             .tasks(.action(.delegate(let value))),
             .fleet(.action(.delegate(let value))),
             .newRequest(.action(.delegate(let value))),
             .inboxPath(.element(id: _, action: .legacy(.action(.delegate(let value))))),
             .tasksPath(.element(id: _, action: .legacy(.action(.delegate(let value))))),
             .fleetPath(.element(id: _, action: .legacy(.action(.delegate(let value))))),
             .newRequestPath(.element(id: _, action: .legacy(.action(.delegate(let value))))),
             .inboxPath(.element(id: _, action: .review(.actions(.delegate(let value))))),
             .tasksPath(.element(id: _, action: .review(.actions(.delegate(let value))))),
             .fleetPath(.element(id: _, action: .review(.actions(.delegate(let value))))),
             .newRequestPath(.element(id: _, action: .review(.actions(.delegate(let value))))):
            delegate = value
        case .triage(.delegate(.noLongerPending)):
            return .actionCompleted(String(localized: "That’s no longer pending"))
        default:
            return nil
        }
        return delegate.event
    }
}

public extension ActionFeature.Action.Delegate {
    var event: WorkspaceNotice.Event {
        switch self {
        case .completed(let message): .actionCompleted(message)
        case .failed(let message): .failure(message)
        }
    }
}

public struct WorkspaceNotice: Equatable, Sendable, Identifiable {
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
