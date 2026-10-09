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
        case stop
        case loaded(ViewDocument)
        case missing
        case failed(String)
        /// The observation ended on its own, without an error and without the screen stopping it.
        case ended
    }

    @Dependency(\.viewsClient) var viewsClient
    /// One per view: a screen stopping or reloading its own view never cancels another screen's,
    /// which TCA would otherwise do for every screen below it (a card's detail under the queue).
    private enum CancelID: Hashable { case observe(String) }

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
                        return await send(.missing)
                    } catch is CancellationError where Task.isCancelled {
                        return
                    } catch {
                        return await send(.failed(String(localized: "Couldn’t load this from your Mac. Try again.")))
                    }
                    guard !Task.isCancelled else { return }
                    await send(.ended)
                }
                .cancellable(id: CancelID.observe(state.key.rawValue), cancelInFlight: true)
            case .ended:
                guard state.document == nil else { return .none }
                state.phase = .failed(String(localized: "Couldn’t load this from your Mac. Try again."))
                return .none
            case .stop:
                return .cancel(id: CancelID.observe(state.key.rawValue))
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
