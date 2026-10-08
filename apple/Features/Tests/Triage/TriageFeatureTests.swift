import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraTransport
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
            $0.viewsClient.load = feed.load
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
        XCTAssertEqual(store.state.currentCard?.need, "Choose the welcome copy")
        XCTAssertNotNil(store.state.cards[id: ids.information]?.card)
        XCTAssertEqual(store.state.badge, 4)
        XCTAssertEqual(store.state.queueTitle, "Waiting for You")
    }

    @MainActor
    func testTheIconBadgeIsUnknownUntilTheServerCountsTheQueue() async throws {
        let store = store(try fullFeed())
        XCTAssertNil(store.state.iconBadge, "a fresh launch must not clear the badge a push set")

        await store.send(.task)
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.iconBadge, 4)
    }

    /// David saw "Waiting for You" badged 1 over an empty queue: the badge read the server's count
    /// while the queue left out the card he had just answered.
    @MainActor
    func testTheBadgeCountsExactlyTheCardsTheQueueShows() async throws {
        let clock = TestClock<Duration>()
        let gate = Gate()
        let home = try Fixtures.document("home-ios") { object in
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            sections[0]["card_ids"] = [ids.question]
            object["sections"] = sections
            var status = try XCTUnwrap(object["status"] as? [String: Any])
            status["badge"] = 1
            object["status"] = status
        }
        let store = gated(FeedStub([home, try Fixtures.card(ids.question)]), clock: clock, gate: gate) {
            try Responses.rejected("It already moved on")
        }
        await store.send(.task)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.badge, 1)

        _ = try await answer(store, id: ids.question)
        await Task.megaYield()
        await clock.advance(by: TriageFeature.stampDuration)
        await store.receive(\.stampElapsed)
        XCTAssertEqual(store.state.orderedIDs, [])
        XCTAssertEqual(store.state.badge, 0, "an answered card leaves the badge with the queue")
        XCTAssertEqual(store.state.iconBadge, 0)

        await gate.open()
        await store.receive(\.cards[id: ids.question].actions.settled)
        XCTAssertEqual(store.state.orderedIDs, [ids.question])
        XCTAssertEqual(store.state.badge, 1, "and comes back with it when the daemon refuses the answer")
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
    func testSwipingEveryKindOfCardOnlyMovesOnAndSendsNothing() async throws {
        let permission = try Fixtures.authorizeCard()
        let decision = try Fixtures.crowdedDecision()
        let cards: [(String, String, ViewDocument)] = [
            ("know", ids.information, try Fixtures.card(ids.information)),
            ("decide", "decide-5", decision),
            ("authorize", "perm-1", permission),
            ("review", ids.pullRequest, try Fixtures.card(ids.pullRequest)),
        ]
        for (kind, front, document) in cards {
            for (translation, predicted) in [(120.0, 160.0), (-120, -160), (40, 300)] {
                let clock = TestClock()
                let feed = FeedStub([try Fixtures.home(queue: [front, ids.question]), document, try Fixtures.card(ids.question)])
                let store = store(feed, clock: clock)
                await store.send(.task)
                await store.skipReceivedActions()
                XCTAssertEqual(store.state.currentID, front, kind)

                await swipe(store, clock: clock, translation: translation, predicted: predicted)
                XCTAssertEqual(store.state.currentID, ids.question, "\(kind) \(translation)")
                XCTAssertEqual(feed.requests, [], "\(kind) \(translation): a swipe sends nothing")
                XCTAssertEqual(store.state.skips, 1, kind)
            }
        }
    }

    @MainActor
    func testTheOnlyCardNeverLeavesBySwipingAndIsNotAnswered() async throws {
        let clock = TestClock()
        let feed = FeedStub([try Fixtures.home(queue: [ids.information]), try Fixtures.card(ids.information)])
        let store = store(feed, clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        await swipe(store, clock: clock, translation: -200, predicted: -500)
        XCTAssertEqual(store.state.currentID, ids.information)
        XCTAssertEqual(store.state.skips, 0)
        XCTAssertEqual(feed.requests, [])
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
            $0.continuousClock = TestClock()
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
            $0.continuousClock = TestClock()
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

    @MainActor
    func testOpeningANotificationForACardSettledElsewhereSaysWhyItWent() async throws {
        let waiting = [ids.information, ids.delivery, ids.pullRequest]
        let feed = FeedStub([try Fixtures.home(queue: waiting, focus: ids.question), try Fixtures.document("card-settled-ios-es")]
            + (try waiting.map(Fixtures.card)))
        let store = store(feed)

        await store.send(.focus(ids.question))
        await store.receive(\.delegate.noLongerPending, "Ya respondida")
        XCTAssertEqual(store.state.orderedIDs, waiting)
    }

    @MainActor
    func testACardSettledElsewhereLeavesTheQueueSayingWhy() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()

        let settled = try Fixtures.document("card-settled-ios-es")
        await store.send(.cards(.element(id: ids.question, action: .screen(.loaded(settled)))))
        await store.receive(\.delegate.noLongerPending, "Ya respondida")
        XCTAssertEqual(store.state.orderedIDs, [ids.information, ids.delivery, ids.pullRequest])
    }

    // MARK: Live queue (0.2.33): the daemon answers 404 for `home?focus=` of a card no longer waiting

    private func liveFeed() throws -> FeedStub {
        FeedStub([
            try Samples.document("live-home-es"),
            try Samples.document("live-card-permission-es"),
            try Samples.document("live-card-ask-es"),
        ])
    }

    private let liveQueue = ["hold-permission-toolu_01R1rRYBcDDQMBfXcpzvDgMn", "ask-parcel-trust-prompt-1"]

    @MainActor
    func testOpeningANotificationForACardAlreadyGoneShowsTheLiveQueue() async throws {
        let store = store(try liveFeed())

        await store.send(.focus("hold-permission-toolu_01AlreadyAnswered"))
        await store.receive(\.delegate.noLongerPending)
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.screen.key, .home)
        XCTAssertNotNil(store.state.screen.document, "the queue is shown, not 'Connecting to the fleet…'")
        XCTAssertEqual(store.state.orderedIDs, liveQueue)
        XCTAssertEqual(store.state.currentCard?.id, liveQueue[0])
        XCTAssertEqual(store.state.cards[id: liveQueue[1]]?.card?.id, liveQueue[1])
        XCTAssertNil(store.state.loadError)
    }

    @MainActor
    func testAFocusedCardLeavingTheQueueFallsBackToTheWholeQueue() async throws {
        let feed = try liveFeed()
        let focused = liveQueue[0]
        feed.serve(try Fixtures.document("home-ios-es", edit: { object in
            object["key"] = "home?focus=\(focused)"
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            sections[0]["card_ids"] = [focused]
            object["sections"] = sections
        }))
        let store = store(feed)
        await store.send(.focus(focused))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.currentID, focused)

        await store.send(.screen(.missing))
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.screen.key, .home)
        XCTAssertNotNil(store.state.screen.document)
        XCTAssertEqual(store.state.orderedIDs, liveQueue)
    }

    @MainActor
    func testACardThatCannotLoadStaysInPlaceWithoutHoldingUpTheOthers() async throws {
        let clock = TestClock()
        let store = store(FeedStub([try Samples.document("live-home-es"), try Samples.document("live-card-ask-es")]), clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.orderedIDs, liveQueue)
        XCTAssertEqual(store.state.cards[id: liveQueue[0]]?.screen.phase, .missing)
        XCTAssertEqual(store.state.cards[id: liveQueue[1]]?.card?.id, liveQueue[1])

        await store.send(.leaveForLater)
        await clock.advance(by: TriageFeature.leaveDuration)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.currentID, liveQueue[1])
    }

    @MainActor
    func testAQueueTheDaemonDoesNotServeIsAnErrorWithRetryNotAnEndlessSpinner() async throws {
        let store = store(FeedStub())
        await store.send(.task)
        await store.skipReceivedActions()

        XCTAssertNil(store.state.screen.document)
        XCTAssertNotNil(store.state.loadError)
    }

    @MainActor
    func testAQueueThatNeverArrivesBecomesAnErrorWithRetry() async throws {
        let clock = TestClock()
        let store = store(FeedStub(), clock: clock)
        store.dependencies.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }

        await store.send(.task)
        XCTAssertNil(store.state.loadError)
        await clock.advance(by: TriageFeature.loadDeadline)
        await store.skipReceivedActions()

        XCTAssertNotNil(store.state.loadError)
        await store.send(.disappeared)
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
        let action = try XCTUnwrap(store.state.cards[id: id]?.buttons.first)
        await store.send(.cards(.element(id: id, action: .actionTapped(action))))
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
        await clock.advance(by: .milliseconds(899))
        XCTAssertEqual(store.state.orderedIDs, queue)
        await clock.advance(by: .milliseconds(1))
        await store.receive(\.stampElapsed)
        XCTAssertEqual(store.state.orderedIDs, moved)
        XCTAssertEqual(store.state.currentID, ids.information, "the next card takes the answered one's place")
        XCTAssertFalse(store.state.newerQueueWaiting)
    }

    @MainActor
    func testAnAnsweredCardStaysOutWhileLaterViewsStillListIt() async throws {
        let clock = TestClock<Duration>()
        let store = store(try fullFeed(), clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        await Task.megaYield()
        await clock.advance(by: TriageFeature.stampDuration)
        await store.receive(\.stampElapsed)
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question))

        await store.send(.screen(.loaded(try Fixtures.home(queue: queue, version: "vq2"))))
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question), "a view the daemon has not caught up with cannot bring it back")
        await store.send(.screen(.loaded(try Fixtures.home(queue: queue, version: "vq3"))))
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question))

        let moved = [ids.information, ids.delivery, ids.pullRequest]
        await store.send(.screen(.loaded(try Fixtures.home(queue: moved, version: "vq4"))))
        XCTAssertTrue(store.state.answered.isEmpty, "a view without the card ends the wait")
        XCTAssertEqual(store.state.orderedIDs, moved)
    }

    @MainActor
    func testACardTheServerStillListsAfterTheTimeToLiveComesBackWithoutItsStamp() async throws {
        let clock = TestClock<Duration>()
        let store = store(try fullFeed(), clock: clock)
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        await Task.megaYield()
        await clock.advance(by: TriageFeature.stampDuration)
        await store.receive(\.stampElapsed)
        await store.skipReceivedActions()
        await store.send(.screen(.loaded(try Fixtures.home(queue: queue, version: "vq2"))))
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question))

        await clock.advance(by: TriageFeature.answeredTimeToLive)
        await store.receive(\.answeredExpired)
        XCTAssertEqual(store.state.orderedIDs, queue)
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
    func testARefusalBringsTheCardBackWithTheServersSentenceNotItsRawText() async throws {
        let clock = TestClock<Duration>()
        let gate = Gate()
        let store = gated(try fullFeed(), clock: clock, gate: gate) {
            try Responses.refused("PR #12 must merge first: this one builds on it.")
        }
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        await Task.megaYield()
        XCTAssertNotNil(store.state.cards[id: ids.question]?.stamp, "the card shows its stamp at once")

        await gate.open()
        await store.receive(\.cards[id: ids.question].actions.settled)
        await store.receive(\.cards[id: ids.question].actions.response)
        XCTAssertEqual(store.state.orderedIDs, queue, "the card is back")
        XCTAssertEqual(store.state.currentID, ids.question, "in front, where it was answered")
        XCTAssertNil(store.state.cards[id: ids.question]?.stamp)
        XCTAssertEqual(store.state.cards[id: ids.question]?.actions.error, "PR #12 must merge first: this one builds on it.")
        await store.receive(\.cards[id: ids.question].actions.delegate.failed)
    }

    @MainActor
    func testAnAnswerThatNeverGetsAnAnswerIsRolledBackWithAnError() async throws {
        let clock = TestClock<Duration>()
        let store = gated(try fullFeed(), clock: clock, gate: Gate())
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        await Task.megaYield()
        await clock.advance(by: TriageFeature.stampDuration)
        await store.receive(\.stampElapsed)
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question))

        await clock.advance(by: TriageFeature.sendDeadline)
        await store.receive(\.sendTimedOut)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.orderedIDs, queue)
        XCTAssertEqual(store.state.currentID, ids.question)
        XCTAssertNil(store.state.cards[id: ids.question]?.stamp)
        XCTAssertEqual(store.state.cards[id: ids.question]?.actions.error,
                       "Your Mac didn’t confirm it. It may have arrived: try again and it won’t be done twice.")
        XCTAssertTrue(store.state.answered.isEmpty)
    }

    @MainActor
    func testAnAnswerThatCannotBeDeliveredBringsTheCardBackWithAnError() async throws {
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.observe = try! fullFeed().observe
            $0.viewsClient.perform = { _ in throw URLError(.notConnectedToInternet) }
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        XCTAssertEqual(store.state.orderedIDs, queue)
        XCTAssertNil(store.state.cards[id: ids.question]?.stamp)
        XCTAssertEqual(store.state.cards[id: ids.question]?.actions.error,
                       "Couldn’t reach your Mac. Nothing was sent; try again.")
    }

    @MainActor
    func testAnAnswerAppliedWithAResultKindThisAppDoesNotKnowStaysAnswered() async throws {
        let clock = TestClock<Duration>()
        let gate = Gate()
        let store = gated(try fullFeed(), clock: clock, gate: gate) {
            try ContractCodec.decode(ActionResponse.self, from: Data(
                #"{"protocol":1,"outcome":"applied","stamp":"Queued","result":{"kind":"queued","title":"Queued for the Mac."}}"#.utf8))
        }
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        await gate.open()
        await store.receive(\.cards[id: ids.question].actions.delegate.completed, "Queued for the Mac.")
        await clock.advance(by: TriageFeature.stampDuration)
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question))
        XCTAssertNil(store.state.cards[id: ids.question]?.actions.error)
    }

    @MainActor
    func testAMacThatAnswersWithAnErrorBringsTheCardBackSayingNothingChanged() async throws {
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.observe = try! fullFeed().observe
            $0.viewsClient.perform = { _ in throw APIError.server(status: 500, kind: nil, detail: "panicked at store.rs", reasons: []) }
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.skipReceivedActions()

        _ = try await answer(store, id: ids.question)
        XCTAssertEqual(store.state.orderedIDs, queue)
        XCTAssertEqual(store.state.currentID, ids.question)
        XCTAssertEqual(store.state.cards[id: ids.question]?.actions.error,
                       "Your Mac couldn’t do it. Nothing changed; try again.", "never the daemon's raw text")
    }

    @MainActor
    func testAnAnswerTheMacMayHaveGotGoesAgainAsTheSameCommand() async throws {
        let clock = TestClock<Duration>()
        let feed = try fullFeed()
        let calls = LockIsolated(0)
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = { request in
                feed.deliver(request)
                let call = calls.withValue { $0 += 1; return $0 }
                if call == 1 { throw OperationError.outcomeUnknown }
                return try Responses.applied()
            }
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.skipReceivedActions()

        let action = try await answer(store, id: ids.question)
        XCTAssertEqual(store.state.currentID, ids.question, "the card is back")
        XCTAssertEqual(store.state.cards[id: ids.question]?.actions.error,
                       "Your Mac didn’t confirm it. It may have arrived: try again and it won’t be done twice.")

        await store.send(.cards(.element(id: ids.question, action: .actionTapped(action))))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.count, 2)
        XCTAssertEqual(feed.requests[1].commandId, feed.requests[0].commandId, "the Mac does it once")
        XCTAssertEqual(feed.requests[1], feed.requests[0])
        await clock.advance(by: TriageFeature.stampDuration)
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.orderedIDs.contains(ids.question))
    }

    @MainActor
    func testAnAnswerThatNeverLeftGoesAgainAsANewCommand() async throws {
        let feed = try fullFeed()
        let calls = LockIsolated(0)
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = { request in
                feed.deliver(request)
                let call = calls.withValue { $0 += 1; return $0 }
                if call == 1 { throw URLError(.cannotConnectToHost) }
                return try Responses.applied()
            }
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.skipReceivedActions()

        let action = try await answer(store, id: ids.question)
        await store.send(.cards(.element(id: ids.question, action: .actionTapped(action))))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.count, 2)
        XCTAssertNotEqual(feed.requests[1].commandId, feed.requests[0].commandId)
    }

    @MainActor
    func testAnAnswerThatTimedOutGoesAgainAsTheSameCommand() async throws {
        let clock = TestClock<Duration>()
        let feed = try fullFeed()
        let gate = Gate()
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = { request in
                feed.deliver(request)
                if feed.requests.count == 1 { await gate.wait() }
                return try Responses.applied()
            }
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.skipReceivedActions()

        let action = try await answer(store, id: ids.question)
        await Task.megaYield()
        await clock.advance(by: TriageFeature.sendDeadline)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.currentID, ids.question)

        await store.send(.cards(.element(id: ids.question, action: .actionTapped(action))))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.commandId), [feed.requests[0].commandId, feed.requests[0].commandId])
        await gate.open()
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
    func testTheHeaderCountsExactlyTheCardsTheQueueShowsInEveryFixture() async throws {
        for (name, spanish) in [("home-ios", false), ("home-ios-es", true)] {
            let home = try Fixtures.document(name)
            let ids = try XCTUnwrap(home.sections.first?.cardIds)
            let store = store(FeedStub([home]))
            await store.send(.task)
            await store.skipReceivedActions()
            XCTAssertEqual(store.state.header?.waiting, store.state.orderedIDs.count, name)
            let daemonNumbers = home.status.summary.components(separatedBy: " · ").dropLast().map { Int($0.prefix { $0.isNumber }) ?? -1 }
            let header = try XCTUnwrap(store.state.header)
            XCTAssertEqual(daemonNumbers, [header.questions, header.toRead].filter { $0 > 0 }, "\(name): with nothing hidden it reads as the Mac's line")

            let request = ActionRequest(commandId: "c-\(name)", dataEpoch: "", client: .ios, lang: spanish ? "es" : "en", actionId: "accept:x@1")
            await store.send(.handedOff(request, cardID: ids[0]))
            XCTAssertEqual(store.state.orderedIDs.count, ids.count - 1, name)
            XCTAssertEqual(store.state.header?.waiting, store.state.orderedIDs.count, "\(name): a card the queue hides is not counted")
        }
    }

    /// David saw «1 pregunta · 1 por leer» over «Nada te espera»: he had replied to the machine
    /// notice, which hid it although a reply answers nothing, then read the orchestrator's answer.
    @MainActor
    func testReplyingFromACardKeepsItWaitingAndTheHeaderNeverCountsWhatIsHidden() async throws {
        let store = store(try fullFeed())
        await store.send(.task)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.header?.waiting, 4)

        let reply = ActionRequest(commandId: "c-reply", dataEpoch: "", client: .ios, lang: "en", actionId: "msg:view@7")
        await store.send(.handedOff(reply, cardID: ids.question))
        await store.send(.handOffSettled(reply, applied: true))
        XCTAssertEqual(store.state.orderedIDs, queue, "a reply answers nothing: the card still waits")

        let read = ActionRequest(commandId: "c-read", dataEpoch: "", client: .ios, lang: "en", actionId: "read:x@1")
        await store.send(.handedOff(read, cardID: ids.information))
        XCTAssertEqual(store.state.orderedIDs, [ids.question, ids.delivery, ids.pullRequest])
        XCTAssertEqual(store.state.header?.waiting, 3, "the header drops the card the queue hides")
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
    func testStatusComesFromTheQueueAndTheMachinePopoverToggles() async throws {
        let home = try Fixtures.home(queue: [])
        let store = store(FeedStub([home]))
        await store.send(.task)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.status, home.status)
        await store.send(.machinePresented(true))
        XCTAssertTrue(store.state.machinePresented)
        await store.send(.machinePresented(false))
        XCTAssertFalse(store.state.machinePresented)
    }
}
