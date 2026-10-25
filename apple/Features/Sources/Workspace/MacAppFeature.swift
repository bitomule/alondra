import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// The Mac window's root until it moves to the shared features: the sidebar and detail presentation
/// plus the toasts and settings sheet around it.
@Reducer
public struct MacAppFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State {
        public init() {}
        public var settingsPresented = false
        /// Whether the Mac is answering: every connection indicator in the window derives from it.
        public var connection = ConnectionState.connecting
        public var notice: WorkspaceNotice?
        /// Answers set aside with a store this Mac could not read; shown until dismissed.
        public var setAside: SetAsideAnswers?
        public var macPresentation = MacPresentationFeature.State()
    }

    public enum Action {
        case appeared, disappeared, setAsideDismissed
        case settingsPresented(Bool)
        case dismissNotice(UUID)
        case macPresentation(MacPresentationFeature.Action)
        case viewsFeed(FeedEvent)
        case connectionChanged(ConnectionState)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    private enum CancelID { case feed, notice, connection }

    public var body: some ReducerOf<Self> {
        Scope(state: \.macPresentation, action: \.macPresentation) { MacPresentationFeature() }
        Reduce { state, action in
            switch action {
            case .dismissNotice(let id):
                guard state.notice?.id == id else { return .none }
                state.notice = nil
                return .cancel(id: CancelID.notice)
            case .setAsideDismissed:
                state.setAside = nil
                return .run { [viewsClient] _ in viewsClient.dismissSetAsideAnswers() }
            case .appeared:
                state.setAside = viewsClient.setAsideAnswers()
                return .merge(
                    .run { [viewsClient] send in
                        for await event in await viewsClient.feed() { await send(.viewsFeed(event)) }
                    }.cancellable(id: CancelID.feed, cancelInFlight: true),
                    .run { [viewsClient] send in
                        for await connection in await viewsClient.connection() { await send(.connectionChanged(connection)) }
                    }.cancellable(id: CancelID.connection, cancelInFlight: true),
                    .send(.macPresentation(.appeared))
                )
            case .disappeared:
                return .merge(.cancel(id: CancelID.feed), .cancel(id: CancelID.connection), .send(.macPresentation(.disappeared)))
            case .settingsPresented(let value):
                state.settingsPresented = value
                return .none
            case .viewsFeed(.stale(let keys)):
                return .send(.macPresentation(.viewsStale(keys)))
            case .viewsFeed(.reset):
                return .send(.macPresentation(.viewsReset))
            case .viewsFeed(.snapshot(let snapshot)):
                return snapshot.reset ? .send(.viewsFeed(.reset)) : .none
            case .macPresentation(.action(.delegate(let delegate))):
                let notice = WorkspaceNotice(event: delegate.event)
                state.notice = notice
                return .run { send in
                    try await clock.sleep(for: .seconds(notice.failure ? 4 : 2.2))
                    await send(.dismissNotice(notice.id))
                }.cancellable(id: CancelID.notice, cancelInFlight: true)
            case .macPresentation(.settled(let why)):
                let notice = WorkspaceNotice(event: .actionCompleted(why))
                state.notice = notice
                return .run { send in
                    try await clock.sleep(for: .seconds(2.2))
                    await send(.dismissNotice(notice.id))
                }.cancellable(id: CancelID.notice, cancelInFlight: true)
            case .connectionChanged(let connection):
                let reconnected = connection.isConnected && !state.connection.isConnected
                state.connection = connection
                return reconnected ? .send(.macPresentation(.reconnected)) : .none
            case .macPresentation(.answerNotConfirmed(let message)):
                let notice = WorkspaceNotice(event: .failure(message))
                state.notice = notice
                return .run { send in
                    try await clock.sleep(for: .seconds(6))
                    await send(.dismissNotice(notice.id))
                }.cancellable(id: CancelID.notice, cancelInFlight: true)
            case .macPresentation:
                return .none
            }
        }
    }
}
