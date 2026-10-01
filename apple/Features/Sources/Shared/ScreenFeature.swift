import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// Loads one server view and keeps it current: the load and refresh plumbing every screen
/// embeds, with no rendering. The screen starts it from `.task`, so it stops when the screen leaves.
@Reducer
public struct ScreenFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public enum Phase: Equatable, Sendable {
            case loading
            case loaded
            case missing
            case failed(String)
        }

        public let key: ViewKey
        public var document: ViewDocument?
        public var phase = Phase.loading

        public init(key: ViewKey) { self.key = key }
    }

    public enum Action: Equatable {
        case task
        case retry
        case loaded(ViewDocument)
        case missing
        case failed(String)
    }

    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID { case observe }

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .task, .retry:
                if state.document == nil { state.phase = .loading }
                return .run { [viewsClient, key = state.key] send in
                    do {
                        for try await fetch in viewsClient.observe(key) {
                            if case .document(let document) = fetch { await send(.loaded(document)) }
                        }
                    } catch ViewsClientError.missingView {
                        await send(.missing)
                    } catch is CancellationError {
                        return
                    } catch {
                        await send(.failed(String(localized: "Couldn’t load this from your Mac. Try again.")))
                    }
                }
                .cancellable(id: CancelID.observe, cancelInFlight: true)
            case .loaded(let document):
                guard document.key == state.key.rawValue else { return .none }
                state.document = document
                state.phase = .loaded
                return .none
            case .missing:
                state.document = nil
                state.phase = .missing
                return .none
            case .failed(let message):
                state.phase = .failed(message)
                return .none
            }
        }
    }
}
