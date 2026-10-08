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
        public var notice: WorkspaceNotice?
        public var macError: WorkspaceNotice?
        public var macPresentation = MacPresentationFeature.State()
    }

    public enum Action {
        case appeared, disappeared
        case settingsPresented(Bool)
        case dismissNotice(UUID), dismissMacError(UUID)
        case macPresentation(MacPresentationFeature.Action)
        case viewsFeed(FeedEvent)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    private enum CancelID { case feed, notice, macError }

    public var body: some ReducerOf<Self> {
        Scope(state: \.macPresentation, action: \.macPresentation) { MacPresentationFeature() }
        Reduce { state, action in
            switch action {
            case .dismissNotice(let id):
                guard state.notice?.id == id else { return .none }
                state.notice = nil
                return .cancel(id: CancelID.notice)
            case .dismissMacError(let id):
                guard state.macError?.id == id else { return .none }
                state.macError = nil
                return .cancel(id: CancelID.macError)
            case .appeared:
                return .run { [viewsClient] send in
                    for await event in await viewsClient.feed() { await send(.viewsFeed(event)) }
                }.cancellable(id: CancelID.feed, cancelInFlight: true)
            case .disappeared:
                return .cancel(id: CancelID.feed)
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
            case .macPresentation(.viewFailed(_, let message)):
                let notice = WorkspaceNotice(event: .failure(message))
                state.macError = notice
                return .run { send in
                    try await clock.sleep(for: .seconds(4))
                    await send(.dismissMacError(notice.id))
                }.cancellable(id: CancelID.macError, cancelInFlight: true)
            case .macPresentation:
                return .none
            }
        }
    }
}
