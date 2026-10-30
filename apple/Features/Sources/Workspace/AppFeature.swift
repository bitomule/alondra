import AlondraContract
import AlondraFeed
import AlondraLiveActivity
import ComposableArchitecture
import Foundation

/// The iPhone's root: three tabs, each with its own stack of pushed screens, the settings sheet, the
/// toast, where a tapped notification lands, and the delivery of what a screen sent as it left. It keeps
/// the Waiting for You queue observed on every tab, since its badge counts that queue, and subscribes
/// to the feed for the screens that still refresh through the root (`legacy`); the rebuilt screens
/// observe their own key.
@Reducer
public struct AppFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public init() {}
        public enum Tab: Hashable, CaseIterable, Sendable { case inbox, tasks, newRequest }
        public var tab = Tab.inbox
        /// Whether the Mac is answering: every connection indicator in the app derives from it.
        public var connection = ConnectionState.connecting
        public var settingsPresented = false
        public var notice: WorkspaceNotice?
        public var triage = TriageFeature.State()
        public var tasks = TasksFeature.State()
        public var newRequest = NewRequestFeature.State()
        public var inboxPath = StackState<DetailRoute.State>()
        public var tasksPath = StackState<DetailRoute.State>()
        public var newRequestPath = StackState<DetailRoute.State>()
        @Presents public var mcpSignIn: McpSignInFeature.State?
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
        /// An `alondra://` link was opened, as from the Live Activity: its tab shows, from its root.
        case linkOpened(AlondraLink)
        case dismissNotice(UUID)
        case triage(TriageFeature.Action)
        case tasks(TasksFeature.Action)
        case newRequest(NewRequestFeature.Action)
        case inboxPath(StackActionOf<DetailRoute>)
        case tasksPath(StackActionOf<DetailRoute>)
        case newRequestPath(StackActionOf<DetailRoute>)
        case viewsFeed(FeedEvent)
        case connectionChanged(ConnectionState)
        case handOffAnswered(ActionRequest, ActionResponse)
        case handOffFailed(ActionRequest, SendFailure)
        /// Connect on the card of the tasks that wait for an MCP server.
        case mcpSignInRequested(card: String, server: String)
        case mcpReconnectRequested(card: String)
        case mcpReconnected(McpSignInFeature.Failure?)
        case mcpSignIn(PresentationAction<McpSignInFeature.Action>)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    @Dependency(\.mcpSignIn) var mcpSignIn
    private enum CancelID { case feed, queue, notice, connection }

    public var body: some ReducerOf<Self> {
        Scope(state: \.triage, action: \.triage) { TriageFeature() }
        Scope(state: \.tasks, action: \.tasks) { TasksFeature() }
        Scope(state: \.newRequest, action: \.newRequest) { NewRequestFeature() }
        Reduce { state, action in
            guard let toast = Self.toast(for: action) else { return reduce(&state, action) }
            let notice = WorkspaceNotice(event: toast)
            state.notice = notice
            return .merge(reduce(&state, action), .run { send in
                try await clock.sleep(for: .seconds(notice.failure ? 4 : 2.2))
                await send(.dismissNotice(notice.id))
            }.cancellable(id: CancelID.notice, cancelInFlight: true))
        }
        .forEach(\.inboxPath, action: \.inboxPath)
        .forEach(\.tasksPath, action: \.tasksPath)
        .forEach(\.newRequestPath, action: \.newRequestPath)
        .ifLet(\.$mcpSignIn, action: \.mcpSignIn) { McpSignInFeature() }
    }

    private func reduce(_ state: inout State, _ action: Action) -> Effect<Action> {
        switch action {
        case .appeared:
            return .merge(
                .run { [viewsClient] send in
                    for await event in await viewsClient.feed() { await send(.viewsFeed(event)) }
                }.cancellable(id: CancelID.feed, cancelInFlight: true),
                .run { [viewsClient] send in
                    for await connection in await viewsClient.connection() { await send(.connectionChanged(connection)) }
                }.cancellable(id: CancelID.connection, cancelInFlight: true),
                .run { [viewsClient] send in
                    for try await fetch in viewsClient.observe(.home) {
                        if case .document(let document) = fetch { await send(.triage(.screen(.loaded(document)))) }
                    }
                } catch: { _, _ in }.cancellable(id: CancelID.queue, cancelInFlight: true)
            )
        case .disappeared:
            return .merge(.send(.triage(.disappeared)), .cancel(id: CancelID.feed), .cancel(id: CancelID.queue),
                          .cancel(id: CancelID.connection))
        case .connectionChanged(let connection):
            state.connection = connection
            return .none
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
        case .notificationOpened(let cardID):
            state.tab = .inbox
            state.settingsPresented = false
            state.inboxPath.removeAll()
            let publicID = String(cardID.split(separator: ":").last ?? Substring(cardID))
            return .send(.triage(.focus(publicID)))
        case .linkOpened(let link):
            state.settingsPresented = false
            switch link {
            case .inbox:
                state.tab = .inbox
                state.inboxPath.removeAll()
            case .tasks:
                state.tab = .tasks
                state.tasksPath.removeAll()
            }
            return .none
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
            case .newRequest: Self.push(key, onto: &state.newRequestPath, owner: "ios.new_request")
            }
            return .none
        case .viewsFeed(.stale(let keys)):
            return Self.refreshPaths(state, .stale(keys))
        case .viewsFeed(.reset):
            return Self.refreshPaths(state, .reset)
        case .viewsFeed(.snapshot(let snapshot)):
            return snapshot.reset ? .send(.viewsFeed(.reset)) : .none
        case .newRequest(.delegate(.open(let key))):
            Self.push(key, onto: &state.newRequestPath, owner: "ios.new_request")
            return .none
        case .tasks(.delegate(.open(let key))):
            Self.push(key, onto: &state.tasksPath, owner: "ios.tasks")
            return .none
        case .inboxPath(.element(id: let id, action: let route)):
            if case .handOff(let request)? = route.delegate { return deliver(request, from: state.inboxPath[id: id]?.key) }
            if case .open(let key)? = route.delegate { Self.push(key, onto: &state.inboxPath, owner: "ios.detail") }
            return .none
        case .tasksPath(.element(id: let id, action: let route)):
            if case .handOff(let request)? = route.delegate { return deliver(request, from: state.tasksPath[id: id]?.key) }
            if case .open(let key)? = route.delegate { Self.push(key, onto: &state.tasksPath, owner: "ios.tasks") }
            return .none
        case .newRequestPath(.element(id: let id, action: let route)):
            if case .handOff(let request)? = route.delegate { return deliver(request, from: state.newRequestPath[id: id]?.key) }
            if case .open(let key)? = route.delegate { Self.push(key, onto: &state.newRequestPath, owner: "ios.new_request") }
            return .none
        case .handOffAnswered(let request, let reply):
            return .send(.triage(.handOffSettled(request, applied: reply.outcome == .applied)))
        case .handOffFailed(let request, let failure):
            return .send(.triage(.handOffUndelivered(request, failure)))
        case let .mcpSignInRequested(card, server):
            state.mcpSignIn = McpSignInFeature.State(card: card, server: server)
            return .none
        case .mcpReconnectRequested(let card):
            return .run { [mcpSignIn] send in
                do {
                    try await mcpSignIn.reconnect(card)
                    await send(.mcpReconnected(nil))
                } catch { await send(.mcpReconnected(McpSignInFeature.failure(of: error, opening: true))) }
            }
        case .triage, .tasks, .newRequest, .inboxPath, .tasksPath, .newRequestPath, .mcpReconnected, .mcpSignIn:
            return .none
        }
    }

    /// A screen that left as it sent its action, before preparing it: the queue shows the result
    /// at once, and the action is prepared, delivered and settled here, where it outlives
    /// the screen.
    private func deliver(_ request: ActionRequest, from key: ViewKey?) -> Effect<Action> {
        let cardID = key.flatMap { key in
            ["card:", "pull_request:"].first { key.rawValue.hasPrefix($0) }.map { String(key.rawValue.dropFirst($0.count)) }
        }
        return .merge(
            cardID.map { .send(.triage(.handedOff(request, cardID: $0))) } ?? .none,
            .run { [viewsClient] send in
                let prepared: ActionRequest
                do { prepared = try await viewsClient.prepare(request) }
                catch { return await send(.handOffFailed(request, SendFailure(preparation: error))) }
                do { await send(.handOffAnswered(prepared, try await viewsClient.perform(prepared))) }
                catch { await send(.handOffFailed(prepared, SendFailure(error))) }
            }
        )
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
            + legacyIDs(state.newRequestPath).map { .send(.newRequestPath(.element(id: $0, action: .legacy(action)))) }
        )
    }

    private static func toast(for action: Action) -> WorkspaceNotice.Event? {
        let delegate: ActionFeature.Action.Delegate
        switch action {
        case .triage(.cards(.element(id: _, action: .actions(.delegate(let value))))):
            delegate = value
        case .tasks(.delegate(let value)), .newRequest(.delegate(let value)):
            return value.event
        case .triage(.delegate(.noLongerPending(let why))):
            return .actionCompleted(why ?? String(localized: "That’s no longer pending"))
        case .triage(.delegate(.answerNotConfirmed(let message))):
            return .failure(message)
        case .handOffAnswered(_, let reply):
            switch OutcomeStyle(reply) {
            case .success(let message): return .actionCompleted(message)
            case .failure(let message): return .failure(message)
            case .ask(let changed): return .failure(changed.title)
            }
        case .handOffFailed(_, let failure):
            return .failure(failure.message)
        case .mcpReconnected(let failure):
            return McpSignInFeature.reconnectNotice(failure)
        case .mcpSignIn(.presented(.delegate(.signedIn(let server)))):
            return McpSignInFeature.signedInNotice(server)
        case .inboxPath(.element(id: _, action: let route)),
             .tasksPath(.element(id: _, action: let route)),
             .newRequestPath(.element(id: _, action: let route)):
            return route.delegate?.event
        default:
            return nil
        }
        return delegate.event
    }
}

public extension ScreenDelegate {
    var event: WorkspaceNotice.Event? {
        switch self {
        case .open: nil
        case .completed(let message): .actionCompleted(message)
        case .failed(let message): .failure(message)
        case .handOff: nil
        }
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
