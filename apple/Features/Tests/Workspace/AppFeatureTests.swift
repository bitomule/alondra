import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class AppFeatureTests: XCTestCase {
    private let clock = TestClock()

    @MainActor
    private func makeStore(_ state: AppFeature.State = AppFeature.State()) -> TestStoreOf<AppFeature> {
        let store = TestStore(initialState: state) {
            AppFeature()
        } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.load = { _, _ in throw ViewsClientError.missingView }
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.closeScreen = { _ in }
        }
        store.exhaustivity = .off
        return store
    }

    private func key(_ raw: String) -> ViewKey { ViewKey(rawValue: raw) }

    // MARK: Notifications

    @MainActor
    func testATappedNotificationShowsItsCardAtTheFrontOfWaitingForYou() async {
        var state = AppFeature.State()
        state.tab = .tasks
        let store = makeStore(state)

        await store.send(.notificationOpened(cardID: "card:a98d034b"))
        await store.receive(\.triage.focus, "a98d034b")
        XCTAssertEqual(store.state.tab, .inbox)

        await store.send(.notificationOpened(cardID: "i-7"))
        await store.receive(\.triage.focus, "i-7")
    }

    @MainActor
    func testATappedNotificationClosesWhateverWasOpenOverWaitingForYou() async {
        var state = AppFeature.State()
        state.tab = .tasks
        state.settingsPresented = true
        let store = makeStore(state)
        await store.send(.open(.inbox, key("task:t-1")))

        await store.send(.notificationOpened(cardID: "i-7"))
        XCTAssertEqual(store.state.tab, .inbox)
        XCTAssertFalse(store.state.settingsPresented)
        XCTAssertTrue(store.state.inboxPath.isEmpty)
    }

    @MainActor
    func testACardThatIsNoLongerPendingSaysSoInAToast() async {
        let store = makeStore()
        await store.send(.triage(.delegate(.noLongerPending(nil))))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("That’s no longer pending"))
    }

    @MainActor
    func testACardSettledElsewhereSaysWhatHappenedInTheToast() async {
        let store = makeStore()
        await store.send(.triage(.delegate(.noLongerPending("Ya aceptada"))))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("Ya aceptada"))
    }

    @MainActor
    func testOpeningFromACardInWaitingForYouPushesItsScreenAsTheOnlyOne() async {
        let store = makeStore()
        await store.send(.open(.inbox, key("task:t-1")))
        await store.send(.triage(.delegate(.open(key("card:i-1")))))
        await store.receive(\.openFromTriage)
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("card:i-1")])
    }

    @MainActor
    func testACardOpenFromWaitingForYouStaysOpenWhenAnotherArrives() async throws {
        let store = makeStore()
        await store.send(.triage(.screen(.loaded(try Fixtures.home(queue: [Fixtures.deliveryID])))))
        await store.send(.openFromTriage(key("card:\(Fixtures.deliveryID)")))

        await store.send(.triage(.screen(.loaded(try Fixtures.home(queue: [Fixtures.questionID, Fixtures.deliveryID], version: "arrived")))))
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("card:\(Fixtures.deliveryID)")])
        XCTAssertEqual(store.state.triage.currentID, Fixtures.deliveryID)
        XCTAssertEqual(store.state.triage.arrivals, [Fixtures.questionID])
    }

    @MainActor
    func testTheReviewIsTheScreenForACardAndItsLinksOpenOnTheSameTab() async throws {
        let store = makeStore()
        await store.send(.open(.tasks, key("card:i-9")))
        let id = try XCTUnwrap(store.state.tasksPath.ids.first)
        guard case .cardDetail = store.state.tasksPath[id: id] else { return XCTFail("a card opens the card detail") }

        await store.send(.tasksPath(.element(id: id, action: .cardDetail(.delegate(.open(key("task:t-1")))))))
        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("card:i-9"), key("task:t-1")])
        XCTAssertTrue(store.state.inboxPath.isEmpty)
    }

    // MARK: Navigation

    @MainActor
    func testATriageCardReplacesWhateverWasPushedInWaitingForYou() async {
        let store = makeStore()
        await store.send(.open(.inbox, key("task:t-1")))
        await store.send(.open(.inbox, key("card:i-1")))
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("task:t-1"), key("card:i-1")])

        await store.send(.openFromTriage(key("card:i-9")))
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("card:i-9")])
    }

    @MainActor
    func testEachTabKeepsItsOwnStack() async {
        let store = makeStore()
        await store.send(.open(.tasks, key("task:t-1")))
        await store.send(.open(.newRequest, key("task:t-2")))

        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("task:t-1")])
        XCTAssertEqual(store.state.newRequestPath.map(\.key), [key("task:t-2")])
        XCTAssertTrue(store.state.inboxPath.isEmpty)
    }

    @MainActor
    func testTheSameScreenIsNotPushedTwiceAndAnUnknownKeyOpensNothing() async {
        let store = makeStore()
        await store.send(.open(.tasks, key("task:t-1")))
        await store.send(.open(.tasks, key("task:t-1")))
        await store.send(.open(.tasks, key("unknown:1")))
        await store.send(.open(.tasks, key("home")))
        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("task:t-1")])
    }

    // MARK: Toast

    @MainActor
    func testACompletedActionShowsAToastThatLeavesAfterTwoPointTwoSeconds() async {
        var state = AppFeature.State()
        state.triage.cards = [TriageCardFeature.State(id: "c-1")]
        let store = makeStore(state)
        await store.send(.triage(.cards(.element(id: "c-1", action: .actions(.delegate(.completed("Answered")))))))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("Answered"))

        await clock.advance(by: .seconds(2))
        XCTAssertNotNil(store.state.notice)
        await clock.advance(by: .seconds(2.2))
        await store.receive(\.dismissNotice)
        XCTAssertNil(store.state.notice)
    }

    @MainActor
    func testAFailureStaysLongerAndANewerToastReplacesTheOlderOne() async {
        let store = makeStore()
        await store.send(.tasks(.delegate(.failed("Couldn’t send. Try again."))))
        XCTAssertEqual(store.state.notice?.event, .failure("Couldn’t send. Try again."))
        await clock.advance(by: .seconds(3))
        XCTAssertNotNil(store.state.notice)

        await store.send(.newRequest(.delegate(.completed("Done"))))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("Done"))
        await clock.advance(by: .seconds(2.2))
        await store.receive(\.dismissNotice)
        XCTAssertNil(store.state.notice, "the first toast's timer must not outlive its replacement")
    }

    @MainActor
    func testToastsComeFromScreensPushedOnAnyTab() async throws {
        let delegate = ViewsScreenFeature.Action.action(.delegate(.failed("No")))
        for tab in AppFeature.State.Tab.allCases {
            let store = makeStore()
            await store.send(.open(tab, key("tasks?section=finished&after=20")))
            switch tab {
            case .inbox:
                let id = try XCTUnwrap(store.state.inboxPath.ids.first)
                await store.send(.inboxPath(.element(id: id, action: .legacy(delegate))))
            case .tasks:
                let id = try XCTUnwrap(store.state.tasksPath.ids.first)
                await store.send(.tasksPath(.element(id: id, action: .legacy(delegate))))
            case .newRequest:
                let id = try XCTUnwrap(store.state.newRequestPath.ids.first)
                await store.send(.newRequestPath(.element(id: id, action: .legacy(delegate))))
            }
            XCTAssertEqual(store.state.notice?.event, .failure("No"), "\(tab)")
        }
    }

    @MainActor
    func testToastsComeFromTheRebuiltScreensOnAnyTab() async throws {
        let routes: [(String, DetailRoute.Action)] = [
            ("task:t-1", .taskDetail(.delegate(.failed("No")))),
            ("schedules", .schedules(.delegate(.failed("No")))),
            ("schedule:new", .scheduleEditor(.delegate(.failed("No")))),
            ("pull_request:github.com/o/r#1", .prReview(.delegate(.failed("No")))),
        ]
        for tab in AppFeature.State.Tab.allCases {
            for (screen, action) in routes {
                let store = makeStore()
                await store.send(.open(tab, key(screen)))
                switch tab {
                case .inbox: await store.send(.inboxPath(.element(id: try XCTUnwrap(store.state.inboxPath.ids.first), action: action)))
                case .tasks: await store.send(.tasksPath(.element(id: try XCTUnwrap(store.state.tasksPath.ids.first), action: action)))
                case .newRequest: await store.send(.newRequestPath(.element(id: try XCTUnwrap(store.state.newRequestPath.ids.first), action: action)))
                }
                XCTAssertEqual(store.state.notice?.event, .failure("No"), "\(screen) on \(tab)")
            }
        }
    }

    @MainActor
    func testAScreenAsksForAnotherAndItIsPushedOnItsOwnTab() async throws {
        let store = makeStore()
        await store.send(.tasks(.delegate(.open(key("schedules")))))
        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("schedules")])

        let schedules = try XCTUnwrap(store.state.tasksPath.ids.first)
        await store.send(.tasksPath(.element(id: schedules, action: .schedules(.delegate(.open(key("schedule:new")))))))
        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("schedules"), key("schedule:new")])

        await store.send(.open(.inbox, key("task:t-1")))
        let task = try XCTUnwrap(store.state.inboxPath.ids.first)
        await store.send(.inboxPath(.element(id: task, action: .taskDetail(.delegate(.open(key("card:i-1")))))))
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("task:t-1"), key("card:i-1")])
        XCTAssertEqual(store.state.tasksPath.count, 2, "the other tabs are not touched")
    }

    @MainActor
    func testAScreenCannotOpenAKeyTheAppDoesNotKnow() async {
        let store = makeStore()
        await store.send(.tasks(.delegate(.open(key("unknown:1")))))
        XCTAssertTrue(store.state.tasksPath.isEmpty)
    }

    @MainActor
    func testTheReviewReportsItsActionsAsToasts() async throws {
        let store = makeStore()
        await store.send(.open(.inbox, key("card:i-9")))
        let id = try XCTUnwrap(store.state.inboxPath.ids.first)
        await store.send(.inboxPath(.element(id: id, action: .cardDetail(.actions(.delegate(.completed("Accepted")))))))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("Accepted"))
    }

    // MARK: Hand-off

    @MainActor
    func testAnActionSentFromAReviewThatLeftIsDeliveredByTheAppAndARefusalSaysWhyWithoutBringingTheCardBack() async throws {
        let gate = Gate()
        let cards = ["c-1", "c-2"]
        var state = AppFeature.State()
        state.triage.screen.document = try Fixtures.home(queue: cards)
        state.triage.orderedIDs = cards
        let store = makeStore(state)
        store.dependencies.viewsClient.observe = { _ in AsyncThrowingStream { $0.finish() } }
        let performed = LockIsolated<[ActionRequest]>([])
        store.dependencies.viewsClient.perform = { request in
            performed.withValue { $0.append(request) }
            await gate.wait()
            return try Responses.rejected("This card has changed")
        }
        await store.send(.open(.inbox, key("card:c-1")))
        let id = try XCTUnwrap(store.state.inboxPath.ids.first)
        let request = ActionRequest(commandId: "c", dataEpoch: try XCTUnwrap(store.state.triage.screen.document).dataEpoch, client: .ios, lang: "en", actionId: "accept:c-1@1")

        await store.send(.inboxPath(.element(id: id, action: .cardDetail(.delegate(.handOff(request))))))
        await store.receive(\.triage.handedOff)
        XCTAssertEqual(store.state.triage.orderedIDs, ["c-2"], "the card leaves Waiting for You at once")

        await gate.open()
        await store.receive(\.handOffAnswered)
        XCTAssertEqual(performed.value, [request])
        await store.receive(\.triage.handOffSettled)
        XCTAssertEqual(store.state.triage.orderedIDs, ["c-2"], "never back as unanswered")
        XCTAssertEqual(store.state.triage.currentID, "c-2")
        XCTAssertEqual(store.state.notice?.event, .failure("This card has changed"))
    }

    @MainActor
    func testAnAnswerFromACardScreenTheMacNeverConfirmsComesBackToWaitingForYouWithAToast() async throws {
        let gate = Gate()
        let cards = ["c-1", "c-2"]
        var state = AppFeature.State()
        state.triage.screen.document = try Fixtures.home(queue: cards)
        state.triage.orderedIDs = cards
        let store = makeStore(state)
        store.dependencies.viewsClient.observe = { _ in AsyncThrowingStream { $0.finish() } }
        let prepared = LockIsolated<[ActionRequest]>([])
        store.dependencies.viewsClient.prepare = { request in
            prepared.withValue { $0.append(request) }
            return request
        }
        store.dependencies.viewsClient.perform = { _ in
            await gate.wait()
            return try Responses.applied()
        }
        await store.send(.open(.inbox, key("card:c-1")))
        let id = try XCTUnwrap(store.state.inboxPath.ids.first)
        let request = ActionRequest(commandId: "c", dataEpoch: try XCTUnwrap(store.state.triage.screen.document).dataEpoch, client: .ios, lang: "en", actionId: "accept:c-1@1")

        await store.send(.inboxPath(.element(id: id, action: .cardDetail(.delegate(.handOff(request))))))
        await store.receive(\.triage.handedOff)
        XCTAssertEqual(store.state.triage.orderedIDs, ["c-2"], "the card leaves Waiting for You at once")
        XCTAssertNil(store.state.notice, "nothing shows while it retries")

        await clock.advance(by: Optimistic.confirmationTimeout)
        await store.receive(\.triage.answerOverdue, "c-1")
        await store.receive(\.triage.delegate.answerNotConfirmed)
        XCTAssertEqual(store.state.triage.orderedIDs, cards, "it waits again")
        XCTAssertEqual(store.state.triage.currentID, "c-1")
        XCTAssertEqual(store.state.notice?.event, .failure("Your Mac hasn’t confirmed an answer."))
        XCTAssertEqual(prepared.value, [request], "the screen hands its answer off before it is prepared")

        await gate.open()
        await store.receive(\.handOffAnswered)
        await store.receive(\.triage.handOffSettled)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.triage.orderedIDs, ["c-2"], "confirmed late, it leaves")
    }

    // MARK: No fleet

    @MainActor
    func testTheTabsAreTheInboxTasksAndANewRequestAndANodeOpensNothing() async {
        XCTAssertEqual(AppFeature.State.Tab.allCases, [.inbox, .tasks, .newRequest])
        let store = makeStore()
        for tab in AppFeature.State.Tab.allCases {
            await store.send(.open(tab, key("node:a-1")))
        }
        XCTAssertTrue(store.state.inboxPath.isEmpty)
        XCTAssertTrue(store.state.tasksPath.isEmpty)
        XCTAssertTrue(store.state.newRequestPath.isEmpty)
    }

    // MARK: Feed

    @MainActor
    func testAChangeReachesTheRootScreensAndOnlyTheScreensStillOnTheLegacyRenderer() async {
        let store = makeStore()
        await store.send(.open(.tasks, key("tasks?section=finished&after=20")))
        await store.send(.open(.inbox, key("tasks?section=active&after=20")))
        await store.send(.open(.inbox, key("card:i-9")))
        await store.send(.open(.newRequest, key("pull_request:github.com/o/r#1")))
        store.exhaustivity = .on

        await store.send(.viewsFeed(.stale(["card:i-9"])))
        await store.receive(\.inboxPath.element)
        await store.receive(\.tasksPath.element)
        XCTAssertEqual(store.state.inboxPath.count, 2, "the review is not sent the change, it observes its own key")
    }

    @MainActor
    func testTheWaitingBadgeFollowsTheQueueFromAnyTab() async throws {
        var state = AppFeature.State()
        state.tab = .tasks
        let queue = [Fixtures.questionID, Fixtures.informationID]
        let feed = FeedStub([try Fixtures.home(queue: queue)])
        let store = TestStore(initialState: state) { AppFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = feed.observe
        }
        store.exhaustivity = .off

        let appeared = await store.send(.appeared)
        await store.receive(\.triage.screen.loaded)
        XCTAssertEqual(store.state.triage.orderedIDs, queue)
        XCTAssertEqual(store.state.triage.badge, 2, "the badge counts the queue without Waiting for You on screen")
        await appeared.cancel()
    }

    @MainActor
    func testARebuiltScreenObservesItselfSoTheRootSendsItNothing() async {
        let store = makeStore()
        for screen in ["task:t-1", "schedules", "schedule:new"] {
            await store.send(.open(.tasks, key(screen)))
        }

        await store.send(.viewsFeed(.stale(["task:t-1", "schedules"])))
        await store.send(.viewsFeed(.reset))
    }
}
