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
    func testTheFrontCardAndTheTwoBehindItAreObservedAndNoOthers() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.orderedIDs, queue)
        XCTAssertEqual(store.state.cards.ids, [ids.question, ids.information, ids.delivery])
        XCTAssertEqual(store.state.currentCard?.title, "Restore the native review experience")
        XCTAssertNotNil(store.state.cards[id: ids.information]?.card)
        XCTAssertEqual(store.state.badge, 4)
        XCTAssertEqual(store.state.queueTitle, "Waiting for You")
    }

    @MainActor
    func testLeavingForLaterSendsTheFrontCardToTheBackOfTheQueueWithoutSendingAnything() async throws {
        let feed = try fullFeed()
        let store = store(feed)
        await store.send(.task)
        await store.skipReceivedActions()

        await store.send(.next)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.currentID, ids.information)
        XCTAssertEqual(store.state.visibleIDs, [ids.information, ids.delivery, ids.pullRequest])
        XCTAssertEqual(store.state.cards.ids.sorted(), [ids.information, ids.delivery, ids.pullRequest].sorted())

        await store.send(.next)
        await store.send(.next)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.visibleIDs, [ids.pullRequest, ids.question, ids.information], "the queue is a ring")
        await store.send(.next)
        XCTAssertEqual(store.state.currentID, ids.question)
        XCTAssertEqual(store.state.skips, 4)
        XCTAssertEqual(feed.requests, [], "browsing marks nothing")
    }

    // MARK: Swiping

    @MainActor
    private func swipe(
        _ store: TestStoreOf<TriageFeature>, clock: TestClock<Duration>, translation: Double, predicted: Double
    ) async {
        await store.send(.dragChanged)
        await store.send(.dragEnded(translation: translation, predicted: predicted))
        await clock.advance(by: TriageFeature.leaveDuration)
        await store.receive(\.swipeSettled)
    }

    @MainActor
    func testSwipingEitherWayPastTheThresholdMovesOnToTheNextCard() async throws {
        let clock = TestClock()
        let feed = try fullFeed()
        let store = store(feed, clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        await store.send(.dragChanged) { $0.swipe = .init(cardID: self.ids.question) }
        await store.send(.dragEnded(translation: -120, predicted: -160)) {
            $0.swipe?.released = true
            $0.swipe?.leaving = .left
        }
        XCTAssertEqual(store.state.currentID, ids.question, "the card leaves before the next one comes forward")
        await clock.advance(by: TriageFeature.leaveDuration)
        await store.receive(\.swipeSettled)
        XCTAssertNil(store.state.swipe)
        XCTAssertEqual(store.state.currentID, ids.information)

        await store.send(.dragChanged)
        await store.send(.dragEnded(translation: 95, predicted: 95)) { $0.swipe?.leaving = .right }
        await clock.advance(by: TriageFeature.leaveDuration)
        await store.receive(\.swipeSettled)
        XCTAssertEqual(store.state.currentID, ids.delivery, "a swipe to the right also moves forward")

        await swipe(store, clock: clock, translation: 40, predicted: 300)
        XCTAssertEqual(store.state.currentID, ids.pullRequest, "a short flick counts")
        XCTAssertEqual(store.state.skips, 3)
        XCTAssertEqual(feed.requests, [], "swiping marks nothing")
    }

    @MainActor
    func testAShortDragSpringsBackAndStaysOnTheCard() async throws {
        let clock = TestClock()
        let store = store(try fullFeed(), clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        for (translation, predicted) in [(-80.0, -200.0), (60, 230), (25, 400), (0, 0)] {
            await store.send(.dragChanged)
            await store.send(.dragEnded(translation: translation, predicted: predicted))
            XCTAssertNil(store.state.swipe?.leaving, "\(translation), \(predicted)")
            await clock.advance(by: TriageFeature.leaveDuration)
            await store.receive(\.swipeSettled)
        }
        XCTAssertEqual(store.state.currentID, ids.question)
        XCTAssertEqual(store.state.skips, 0)
    }

    @MainActor
    func testTheOnlyCardNeverLeaves() async throws {
        let clock = TestClock()
        let store = store(FeedStub([try Fixtures.home(queue: [ids.question]), try Fixtures.card(ids.question)]), clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        await swipe(store, clock: clock, translation: -200, predicted: -500)
        await store.send(.next)
        XCTAssertEqual(store.state.currentID, ids.question)
        XCTAssertEqual(store.state.skips, 0)
    }

    @MainActor
    func testLeaveForLaterSendsTheCardOffLikeASwipe() async throws {
        let clock = TestClock()
        let feed = try fullFeed()
        let store = store(feed, clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        await store.send(.leaveForLater) {
            $0.swipe = .init(cardID: self.ids.question, released: true, leaving: .left)
        }
        await store.send(.leaveForLater)
        await store.send(.dragChanged)
        XCTAssertEqual(store.state.swipe?.leaving, .left, "a drag while the card leaves changes nothing")
        await clock.advance(by: TriageFeature.leaveDuration)
        await store.receive(\.swipeSettled)
        XCTAssertNil(store.state.swipe)
        XCTAssertEqual(store.state.currentID, ids.information)
        XCTAssertEqual(store.state.skips, 1)
        XCTAssertEqual(feed.requests, [], "leaving for later marks nothing")
    }

    @MainActor
    func testTheOnlyCardCannotBeLeftForLater() async throws {
        let store = store(FeedStub([try Fixtures.home(queue: [ids.question]), try Fixtures.card(ids.question)]))
        await store.send(.task)
        await store.skipReceivedActions()

        await store.send(.leaveForLater)
        XCTAssertNil(store.state.swipe)
        XCTAssertEqual(store.state.currentID, ids.question)
    }

    @MainActor
    func testADragNeverOpensTheCard() async throws {
        let clock = TestClock()
        var state = TriageFeature.State()
        state.orderedIDs = queue
        state.cards = [TriageCardFeature.State(id: ids.question)]
        let store = TestStore(initialState: state) { TriageFeature() } withDependencies: {
            $0.continuousClock = clock
        }
        let key = ViewKey.card(ids.question)
        let open = TriageFeature.Action.cards(.element(id: ids.question, action: .opened(key)))

        await store.send(.dragChanged) { $0.swipe = .init(cardID: self.ids.question) }
        await store.send(open)
        await store.receive(\.cards[id: ids.question].delegate.open)
        await store.send(.dragEnded(translation: -20, predicted: -20)) { $0.swipe?.released = true }
        await store.send(open)
        await store.receive(\.cards[id: ids.question].delegate.open)
        await clock.advance(by: TriageFeature.leaveDuration)
        await store.receive(\.swipeSettled) { $0.swipe = nil }

        await store.send(open)
        await store.receive(\.cards[id: ids.question].delegate.open)
        await store.receive(\.delegate.open, key)
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
        state.cards = [TriageCardFeature.State(id: ids.question), TriageCardFeature.State(id: ids.information)]
        let store = TestStore(initialState: state) { TriageFeature() } withDependencies: {
            $0.viewsClient.observe = { $0.rawValue == "card:\(self.ids.question)" ? stream : observe($0) }
        }
        store.exhaustivity = .off

        await store.send(.cards(.element(id: ids.question, action: .screen(.task))))
        await store.send(.next)
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
    func testACardANewerQueueStillListsComesBackWithoutItsStamp() async throws {
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
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question), "the same view cannot bring an applied answer back")

        await store.send(.screen(.loaded(try Fixtures.home(queue: queue, version: "vq2"))))
        XCTAssertEqual(store.state.orderedIDs, queue, "a newer view rules")
        XCTAssertNil(store.state.cards[id: ids.question]?.stamp)
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

    // MARK: Optimistic answers

    @MainActor
    private func gated(
        _ feed: FeedStub, clock: TestClock<Duration>, gate: Gate,
        outcome: @escaping @Sendable () throws -> ActionResponse = { try Responses.applied() }
    ) -> TestStoreOf<TriageFeature> {
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = { request in
                feed.deliver(request)
                await gate.wait()
                return try outcome()
            }
        }
        store.exhaustivity = .off
        return store
    }

    @MainActor
    func testAnAnsweredCardLeavesWhileTheDaemonHasNotAnsweredAndAStaleQueueCannotBringItBack() async throws {
        let clock = TestClock<Duration>()
        let gate = Gate()
        let feed = try fullFeed()
        let store = gated(feed, clock: clock, gate: gate)
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        XCTAssertEqual(store.state.cards[id: ids.question]?.stamp, "Answered", "the stamp is up before the daemon answers")
        await Task.megaYield()
        await clock.advance(by: TriageFeature.stampDuration)
        await store.receive(\.stampElapsed)
        XCTAssertEqual(store.state.orderedIDs, [ids.information, ids.delivery, ids.pullRequest])
        XCTAssertEqual(store.state.currentID, ids.information, "the next card comes forward without waiting")

        await store.send(.screen(.loaded(try Fixtures.home(queue: queue, version: "v-before"))))
        XCTAssertEqual(store.state.orderedIDs, [ids.information, ids.delivery, ids.pullRequest],
                       "a queue rendered before the daemon applied the answer cannot show the card again")

        await gate.open()
        await store.receive(\.cards[id: ids.question].actions.settled)
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question))
        XCTAssertEqual(feed.requests.count, 1)
    }

    @MainActor
    func testAnAnswerTheDaemonDidNotApplyBringsTheCardBackToTheFront() async throws {
        let clock = TestClock<Duration>()
        let gate = Gate()
        let store = gated(try fullFeed(), clock: clock, gate: gate) {
            try Responses.rejected("It already moved on")
        }
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        await Task.megaYield()
        await clock.advance(by: TriageFeature.stampDuration)
        await store.receive(\.stampElapsed)
        XCTAssertEqual(store.state.currentID, ids.information)

        await gate.open()
        await store.receive(\.cards[id: ids.question].actions.settled)
        XCTAssertEqual(store.state.orderedIDs, queue, "the card returns to its place")
        XCTAssertEqual(store.state.currentID, ids.question, "and to the front, where it was answered")
        await store.receive(\.cards[id: ids.question].actions.response)
        XCTAssertNil(store.state.cards[id: ids.question]?.stamp)
        XCTAssertEqual(store.state.cards[id: ids.question]?.actions.error, "It already moved on")
        await store.receive(\.cards[id: ids.question].actions.delegate.failed)
    }

    @MainActor
    func testAnAnswerFromItsOwnScreenTakesTheCardOutAtOnceAndBackIfItFails() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()
        let request = ActionRequest(commandId: "c-1", dataEpoch: "", client: .ios, lang: "en", actionId: "accept:x@1")

        await store.send(.handedOff(request, cardID: ids.delivery))
        XCTAssertEqual(store.state.orderedIDs, [ids.question, ids.information, ids.pullRequest])
        XCTAssertEqual(store.state.currentID, ids.question)

        await store.send(.screen(.loaded(try Fixtures.home(queue: queue))))
        XCTAssertFalse(store.state.orderedIDs.contains(ids.delivery), "a stale queue keeps it out")

        await store.send(.handOffSettled(request, applied: false))
        XCTAssertEqual(store.state.orderedIDs, queue)
        XCTAssertEqual(store.state.currentID, ids.delivery)
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
