import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// Loads one server view and keeps it current: the load and refresh plumbing every screen
/// embeds, with no rendering. The screen starts it from `.task`, so it stops when the screen leaves.
/// A load that fails is tried again on its own, slower each time, and at once when the app
/// retries the connection; what was already shown stays on screen meanwhile. A failure that
/// usually clears by itself is only shown, with its reason, once it keeps happening.
@Reducer
public struct ScreenFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        /// Failures in a row of the kind that clears by itself before the screen shows one.
        public static let passingFailuresShown = 3

        public enum Phase: Equatable, Sendable {
            case loading
            case loaded
            case missing
            case failed(String)
        }

        public let key: ViewKey
        public var document: ViewDocument?
        public var phase = Phase.loading
        public var failures = 0

        public init(key: ViewKey) { self.key = key }
    }

    public enum Action: Equatable {
        case task
        case retry
        case stop
        case loaded(ViewDocument)
        case missing
        case failed(String)
        case loadFailed(LoadFailure)
        /// The observation ended on its own, without an error and without the screen stopping it.
        case ended
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    @Dependency(\.withRandomNumberGenerator) var withRandomNumberGenerator
    /// One per view: a screen stopping or reloading its own view never cancels another screen's,
    /// which TCA would otherwise do for every screen below it (a card's detail under the queue).
    private enum CancelID: Hashable { case observe(String), retry(String) }

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
                        return await send(.loadFailed(LoadFailure(error)))
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
                return .merge(.cancel(id: CancelID.observe(state.key.rawValue)), .cancel(id: CancelID.retry(state.key.rawValue)))
            case .loaded(let document):
                guard document.key == state.key.rawValue else { return .none }
                state.document = document
                state.phase = .loaded
                state.failures = 0
                return .cancel(id: CancelID.retry(state.key.rawValue))
            case .missing:
                state.document = nil
                state.phase = .missing
                return .none
            case .failed(let message):
                state.phase = .failed(message)
                state.failures += 1
                return retryLater(state)
            case .loadFailed(let failure):
                state.failures += 1
                if !failure.isPassing || state.failures >= State.passingFailuresShown {
                    state.phase = .failed(failure.message)
                }
                return retryLater(state)
            }
        }
    }

    /// Waits out the backoff for this many failures, or less if the app retries the connection.
    private func retryLater(_ state: State) -> Effect<Action> {
        let random = withRandomNumberGenerator { Double.random(in: 0...1, using: &$0) }
        let delay = ConnectionBackoff.standard.delay(attempt: state.failures - 1, random: random)
        return .run { [clock, viewsClient] send in
            let reconnections = await viewsClient.reconnections()
            await withTaskGroup(of: Void.self) { group in
                group.addTask { try? await clock.sleep(for: delay) }
                group.addTask { for await _ in reconnections { return } }
                await group.next()
                group.cancelAll()
            }
            guard !Task.isCancelled else { return }
            await send(.retry)
        }
        .cancellable(id: CancelID.retry(state.key.rawValue), cancelInFlight: true)
    }
}
