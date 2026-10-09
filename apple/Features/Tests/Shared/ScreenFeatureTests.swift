import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class ScreenFeatureTests: XCTestCase {
    private struct Boom: Error {}

    private func store(
        observing stream: AsyncThrowingStream<ViewFetch, any Error>
    ) -> TestStoreOf<ScreenFeature> {
        TestStore(initialState: ScreenFeature.State(key: .tasks)) { ScreenFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in stream }
        }
    }

    @MainActor
    func testShowsEachDocumentTheViewDeliversInOrder() async throws {
        let first = try Fixtures.document("tasks-ios", version: "v1")
        let second = try Fixtures.document("tasks-ios", version: "v2")
        let (stream, input) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let store = store(observing: stream)

        await store.send(.task)
        input.yield(.document(first))
        await store.receive(\.loaded) { $0.document = first; $0.phase = .loaded }
        input.yield(.document(second))
        await store.receive(\.loaded) { $0.document = second }
        input.finish()
        await store.receive(\.ended)
        await store.finish()
    }

    @MainActor
    func testIgnoresADocumentThatBelongsToAnotherKey() async throws {
        let fleet = try Fixtures.document("fleet-ios")
        let (stream, input) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let store = store(observing: stream)

        await store.send(.task)
        input.yield(.document(fleet))
        await store.receive(\.loaded)
        input.finish()
        await store.receive(\.ended) { $0.phase = .failed("Couldn’t load this from your Mac. Try again.") }
        await store.finish()
        XCTAssertNil(store.state.document, "a document for another key is never shown, and the screen says it could not load")
    }

    /// The feed cancelling a load under the screen is not the screen leaving: before, the
    /// cancellation was swallowed and the screen kept its placeholder forever.
    @MainActor
    func testACancellationTheScreenDidNotAskForSaysItCouldNotLoad() async throws {
        let (stream, input) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let store = store(observing: stream)

        await store.send(.task)
        input.finish(throwing: CancellationError())
        await store.receive(\.failed) { $0.phase = .failed("Couldn’t load this from your Mac. Try again.") }
        await store.finish()
    }

    /// David's question 2 of 4 stayed on its placeholder. Opening it hid the queue, whose screen
    /// stopped its own view; with one cancel id for every screen, TCA cancelled the detail's
    /// observation too, since every effect below a screen is also registered under it.
    @MainActor
    func testAScreenStoppingItsViewNeverStopsAScreenOpenedBelowIt() async throws {
        let card = try Fixtures.card(Fixtures.questionID)
        let detailKey = ViewKey.card(Fixtures.questionID)
        let (detail, deliver) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let (queue, closeQueue) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        var layers = Layers.State()
        layers.details.append(ScreenFeature.State(key: detailKey))
        let store = TestStore(initialState: layers) { Layers() } withDependencies: {
            $0.viewsClient.observe = { key in key == .home ? queue : detail }
        }
        store.exhaustivity = .off

        await store.send(.queue(.task))
        await store.send(.details(.element(id: detailKey.rawValue, action: .task)))
        await store.send(.queue(.stop))
        deliver.yield(.document(card))
        await store.receive(\.details[id: detailKey.rawValue].loaded)
        XCTAssertEqual(store.state.details[id: detailKey.rawValue]?.phase, .loaded)
        deliver.finish()
        closeQueue.finish()
        await store.skipReceivedActions()
        await store.finish()
    }

    @MainActor
    func testAKeyTheDaemonStoppedServingIsReportedMissingAndDropsTheStaleDocument() async throws {
        let first = try Fixtures.document("tasks-ios")
        let (stream, input) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let store = store(observing: stream)

        await store.send(.task)
        input.yield(.document(first))
        await store.receive(\.loaded) { $0.document = first; $0.phase = .loaded }
        input.finish(throwing: ViewsClientError.missingView)
        await store.receive(\.missing) { $0.document = nil; $0.phase = .missing }
        await store.finish()
    }

    @MainActor
    func testAFailureKeepsWhatWasShownAndRetryObservesAgain() async throws {
        let first = try Fixtures.document("tasks-ios", version: "v1")
        let again = try Fixtures.document("tasks-ios", version: "v2")
        let message = "Couldn’t load this from your Mac. Try again."
        let streams = LockIsolated([
            AsyncThrowingStream<ViewFetch, any Error> { continuation in
                continuation.yield(.document(first))
                continuation.finish(throwing: Boom())
            },
            AsyncThrowingStream<ViewFetch, any Error> { continuation in
                continuation.yield(.document(again))
                continuation.finish()
            },
        ])
        let store = TestStore(initialState: ScreenFeature.State(key: .tasks)) {
            ScreenFeature()
        } withDependencies: {
            $0.viewsClient.observe = { _ in streams.withValue { $0.removeFirst() } }
        }

        await store.send(.task)
        await store.receive(\.loaded) { $0.document = first; $0.phase = .loaded }
        await store.receive(\.failed) { $0.phase = .failed(message) }
        await store.send(.retry)
        await store.receive(\.loaded) { $0.document = again; $0.phase = .loaded }
        await store.receive(\.ended)
        await store.finish()
    }

    @MainActor
    func testLeavingTheScreenStopsObserving() async throws {
        let (stream, input) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let stopped = expectation(description: "the daemon feed for this key is released")
        input.onTermination = { _ in stopped.fulfill() }
        let store = store(observing: stream)

        let observing = await store.send(.task)
        await observing.cancel()
        await fulfillment(of: [stopped], timeout: 10)
    }
}

/// A screen with others opened below it, as the queue has its cards and a card its detail.
@Reducer
private struct Layers {
    @ObservableState
    struct State: Equatable {
        var queue = ScreenFeature.State(key: .home)
        var details = IdentifiedArray<String, ScreenFeature.State>(id: \.key.rawValue)
    }

    enum Action {
        case queue(ScreenFeature.Action)
        case details(IdentifiedAction<String, ScreenFeature.Action>)
    }

    var body: some ReducerOf<Self> {
        Scope(state: \.queue, action: \.queue) { ScreenFeature() }
        EmptyReducer().forEach(\.details, action: \.details) { ScreenFeature() }
    }
}
