import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

final class TriageFeatureTests: XCTestCase {
    private typealias Ids = (question: String, information: String, delivery: String, pullRequest: String)
    private let ids: Ids = (Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID, Fixtures.pullRequestID)
    private var queue: [String] { [ids.question, ids.information, ids.delivery, ids.pullRequest] }

    @MainActor
    private func store(
        _ feed: FeedStub, clock: TestClock<Duration> = TestClock()
    ) -> TestStoreOf<TriageFeature> {
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = feed.perform()
        }
        store.exhaustivity = .off
        return store
    }

    private func fullFeed() throws -> FeedStub {
        FeedStub([try Fixtures.home(queue: queue)] + (try queue.map(Fixtures.card)))
    }

    // MARK: Queue

    @MainActor
    func testTheFrontCardAndItsNeighboursAreObservedAndNoOthers() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.orderedIDs, queue)
        XCTAssertEqual(store.state.cards.ids, [ids.question, ids.information])
        XCTAssertEqual(store.state.currentCard?.title, "Restore the native review experience")
        XCTAssertNotNil(store.state.cards[id: ids.information]?.card)
        XCTAssertEqual(store.state.badge, 4)
        XCTAssertEqual(store.state.queueTitle, "Waiting for You")
    }

    @MainActor
    func testSwipingBrowsesTheQueueWithoutSendingAnything() async throws {
        let feed = try fullFeed()
        let store = store(feed)
        await store.send(.task)
        await store.skipReceivedActions()

        await store.send(.move(1))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.currentID, ids.information)
        XCTAssertEqual(store.state.cards.ids.sorted(), [ids.question, ids.information, ids.delivery].sorted())

        await store.send(.move(2))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.currentID, ids.pullRequest)
        XCTAssertEqual(Set(store.state.cards.ids), [ids.delivery, ids.pullRequest])

        await store.send(.move(5))
        XCTAssertEqual(store.state.index, 3, "swiping past the last card stays on it")
        await store.send(.move(-9))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.index, 0)
        XCTAssertEqual(feed.requests, [], "browsing marks nothing")
    }

    @MainActor
    func testACardThatLeftTheWindowIsNoLongerObserved() async throws {
        let (stream, input) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let stopped = expectation(description: "the first card's feed is released")
        input.onTermination = { _ in stopped.fulfill() }
        let feed = try fullFeed()
        let observe = feed.observe
        var state = TriageFeature.State()
        state.orderedIDs = queue
        state.index = 1
        state.cards = [TriageCardFeature.State(id: ids.question), TriageCardFeature.State(id: ids.information)]
        let store = TestStore(initialState: state) { TriageFeature() } withDependencies: {
            $0.viewsClient.observe = { $0.rawValue == "card:\(self.ids.question)" ? stream : observe($0) }
        }
        store.exhaustivity = .off

        await store.send(.cards(.element(id: ids.question, action: .screen(.task))))
        await store.send(.move(2))
        await fulfillment(of: [stopped], timeout: 10)
        XCTAssertNil(store.state.cards[id: ids.question])
    }

    @MainActor
    func testLeavingTheScreenStopsEveryFeed() async throws {
        let (stream, input) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let stopped = expectation(description: "the home feed is released")
        input.onTermination = { _ in stopped.fulfill() }
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in stream }
        }
        store.exhaustivity = .off

        await store.send(.task)
        await store.send(.disappeared)
        await fulfillment(of: [stopped], timeout: 10)
    }

    // MARK: Focus from a notification

    @MainActor
    func testFocusingAPendingCardShowsItFirstWithoutComplaint() async throws {
        let focused = ids.delivery
        let reordered = [focused, ids.question, ids.information, ids.pullRequest]
        let feed = FeedStub([try Fixtures.home(queue: reordered, focus: focused)] + (try queue.map(Fixtures.card)))
        let store = store(feed)

        await store.send(.focus(focused))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.currentID, focused)
        XCTAssertEqual(store.state.screen.key, .focusedHome(focused))
        XCTAssertNil(store.state.focusedID)
    }

    @MainActor
    func testFocusingACardThatIsGoneSaysItIsNoLongerPending() async throws {
        let feed = FeedStub([try Fixtures.home(queue: queue, focus: "gone")] + (try queue.map(Fixtures.card)))
        let store = store(feed)

        await store.send(.focus("gone"))
        await store.receive(\.delegate.noLongerPending)
        XCTAssertNil(store.state.focusedID)
        XCTAssertEqual(store.state.screen.key, .focusedHome("gone"))
    }

    // MARK: Offline

    @MainActor
    func testLosingTheConnectionKeepsTheQueueAndSaysSo() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.isOffline)

        await store.send(.screen(.failed("offline")))
        XCTAssertTrue(store.state.isOffline)
        XCTAssertEqual(store.state.orderedIDs, queue)
        XCTAssertNil(store.state.loadError, "a saved queue is shown, not an error page")
    }

    @MainActor
    func testWithNothingSavedAFailureIsAnErrorPage() async throws {
        let store = store(FeedStub())
        await store.send(.screen(.failed("offline")))
        XCTAssertEqual(store.state.loadError, "offline")
        XCTAssertFalse(store.state.isOffline)
    }

    // MARK: Answering

    @MainActor
    private func answer(
        _ store: TestStoreOf<TriageFeature>, id: String
    ) async throws -> AlondraContract.Action {
        let action = try XCTUnwrap(store.state.cards[id: id]?.card?.actions.first { $0.presentation == .options })
        let option = try XCTUnwrap(action.options.first)
        await store.send(.cards(.element(id: id, action: .optionPicked(action, optionID: option.id))))
        await store.send(.cards(.element(id: id, action: .replySent(action))))
        await store.skipReceivedActions()
        return action
    }

    @MainActor
    func testAnAnsweredCardShowsItsStampBeforeTheQueueMovesOn() async throws {
        let clock = TestClock<Duration>()
        let feed = try fullFeed()
        let store = store(feed, clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        XCTAssertEqual(feed.requests.count, 1)
        XCTAssertEqual(store.state.cards[id: ids.question]?.stamp, "Answered")

        let moved = [ids.information, ids.delivery, ids.pullRequest]
        await store.send(.screen(.loaded(try Fixtures.home(queue: moved))))
        XCTAssertEqual(store.state.orderedIDs, queue, "the queue waits while the stamp shows")
        XCTAssertTrue(store.state.newerQueueWaiting)

        await Task.megaYield()
        await clock.advance(by: .milliseconds(649))
        XCTAssertEqual(store.state.orderedIDs, queue)
        await clock.advance(by: .milliseconds(1))
        await store.receive(\.stampElapsed)
        XCTAssertEqual(store.state.orderedIDs, moved)
        XCTAssertEqual(store.state.currentID, ids.information, "the next card takes the answered one's place")
        XCTAssertFalse(store.state.newerQueueWaiting)
    }

    @MainActor
    func testACardThatStaysInTheQueueLosesItsStampWhenItEnds() async throws {
        let clock = TestClock<Duration>()
        let store = store(try fullFeed(), clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        XCTAssertNotNil(store.state.cards[id: ids.question]?.stamp)
        await Task.megaYield()
        await clock.advance(by: .milliseconds(650))
        await store.receive(\.stampElapsed)
        await store.skipReceivedActions()
        XCTAssertNil(store.state.cards[id: ids.question]?.stamp)
        XCTAssertEqual(store.state.currentID, ids.question)
    }

    @MainActor
    func testAnAnswerTheServerRefusedShowsNoStampAndKeepsTheQueue() async throws {
        let clock = TestClock<Duration>()
        let feed = try fullFeed()
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = feed.perform(#"{"protocol":1,"outcome":"rejected","message":"It already moved on"}"#)
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        XCTAssertNil(store.state.cards[id: ids.question]?.stamp)
        XCTAssertEqual(store.state.cards[id: ids.question]?.actions.error, "It already moved on")
        await clock.advance(by: .seconds(5))
        XCTAssertEqual(store.state.orderedIDs, queue)
    }

    @MainActor
    func testAnAnswerFromTheFrontCardNeverStampsItsNeighbour() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()
        _ = try await answer(store, id: ids.question)
        XCTAssertNil(store.state.cards[id: ids.information]?.stamp)
    }

    // MARK: Going up

    @MainActor
    func testOpeningFromACardOpensItsScreen() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()
        let key = ViewKey.node("n-1")

        await store.send(.cards(.element(id: ids.question, action: .opened(key))))
        await store.receive(\.cards[id: ids.question].delegate)
        await store.receive(\.delegate.open, key)
    }

    @MainActor
    func testStatusArrivesFromTheAppAndTheMachinePopoverToggles() async throws {
        let store = store(FeedStub())
        let status = try Fixtures.home(queue: []).status
        await store.send(.feedStatus(status))
        XCTAssertEqual(store.state.status, status)
        await store.send(.machinePresented(true))
        XCTAssertTrue(store.state.machinePresented)
        await store.send(.machinePresented(false))
        XCTAssertFalse(store.state.machinePresented)
    }
}
