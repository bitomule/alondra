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
        state.tab = .fleet
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
        await store.send(.triage(.delegate(.noLongerPending)))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("That’s no longer pending"))
    }

    @MainActor
    func testOpeningFromACardInWaitingForYouPushesItsScreenAsTheOnlyOne() async {
        let store = makeStore()
        await store.send(.open(.inbox, key("task:t-1")))
        await store.send(.triage(.delegate(.open(key("node:a-1")))))
        await store.receive(\.openFromTriage)
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("node:a-1")])
    }

    @MainActor
    func testTheReviewIsTheScreenForACardAndItsLinksOpenOnTheSameTab() async throws {
        let store = makeStore()
        await store.send(.open(.tasks, key("card:i-9")))
        let id = try XCTUnwrap(store.state.tasksPath.ids.first)
        guard case .review = store.state.tasksPath[id: id] else { return XCTFail("a card opens the review") }

        await store.send(.tasksPath(.element(id: id, action: .review(.delegate(.open(key("task:t-1")))))))
        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("card:i-9"), key("task:t-1")])
        XCTAssertTrue(store.state.inboxPath.isEmpty)
    }

    // MARK: Navigation

    @MainActor
    func testATriageCardReplacesWhateverWasPushedInWaitingForYou() async {
        let store = makeStore()
        await store.send(.open(.inbox, key("task:t-1")))
        await store.send(.open(.inbox, key("node:a-1")))
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("task:t-1"), key("node:a-1")])

        await store.send(.openFromTriage(key("card:i-9")))
        XCTAssertEqual(store.state.inboxPath.map(\.key), [key("card:i-9")])
    }

    @MainActor
    func testEachTabKeepsItsOwnStack() async {
        let store = makeStore()
        await store.send(.open(.tasks, key("task:t-1")))
        await store.send(.open(.fleet, key("node:a-1")))
        await store.send(.open(.newRequest, key("node:a-2")))

        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("task:t-1")])
        XCTAssertEqual(store.state.fleetPath.map(\.key), [key("node:a-1")])
        XCTAssertEqual(store.state.newRequestPath.map(\.key), [key("node:a-2")])
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

        await store.send(.fleet(.delegate(.screen(.completed("Done")))))
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
            case .fleet:
                let id = try XCTUnwrap(store.state.fleetPath.ids.first)
                await store.send(.fleetPath(.element(id: id, action: .legacy(delegate))))
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
            ("node:a-1", .nodeDetail(.delegate(.failed("No")))),
        ]
        for tab in AppFeature.State.Tab.allCases {
            for (screen, action) in routes {
                let store = makeStore()
                await store.send(.open(tab, key(screen)))
                switch tab {
                case .inbox: await store.send(.inboxPath(.element(id: try XCTUnwrap(store.state.inboxPath.ids.first), action: action)))
                case .tasks: await store.send(.tasksPath(.element(id: try XCTUnwrap(store.state.tasksPath.ids.first), action: action)))
                case .fleet: await store.send(.fleetPath(.element(id: try XCTUnwrap(store.state.fleetPath.ids.first), action: action)))
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

        await store.send(.open(.fleet, key("task:t-1")))
        let task = try XCTUnwrap(store.state.fleetPath.ids.first)
        await store.send(.fleetPath(.element(id: task, action: .taskDetail(.delegate(.open(key("node:a-1")))))))
        XCTAssertEqual(store.state.fleetPath.map(\.key), [key("task:t-1"), key("node:a-1")])
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
        await store.send(.inboxPath(.element(id: id, action: .review(.actions(.delegate(.completed("Accepted")))))))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("Accepted"))
    }

    // MARK: Close refusal

    @MainActor
    func testARefusedCloseFromTheFleetOpensTheRefusalAlert() async {
        let store = makeStore()

        await store.send(.fleet(.delegate(.closeRefused("It is working"))))
        XCTAssertEqual(store.state.closeRefusal, "It is working")
        await store.send(.dismissCloseRefusal)
        XCTAssertNil(store.state.closeRefusal)
    }

    @MainActor
    func testTheFleetOpensANodeAndTheNodeOpensItsTaskInTheFleetStack() async throws {
        let store = makeStore()

        await store.send(.fleet(.delegate(.screen(.open(key("node:a-1"))))))
        let id = try XCTUnwrap(store.state.fleetPath.ids.first)
        await store.send(.fleetPath(.element(id: id, action: .nodeDetail(.delegate(.open(key("task:t-1")))))))

        XCTAssertEqual(store.state.fleetPath.map(\.key), [key("node:a-1"), key("task:t-1")])
        XCTAssertTrue(store.state.inboxPath.isEmpty)
    }

    // MARK: Feed

    @MainActor
    func testAChangeReachesTheRootScreensAndOnlyTheScreensStillOnTheLegacyRenderer() async {
        let store = makeStore()
        await store.send(.open(.tasks, key("tasks?section=finished&after=20")))
        await store.send(.open(.inbox, key("tasks?section=archived&after=20")))
        await store.send(.open(.inbox, key("card:i-9")))
        await store.send(.open(.fleet, key("node:a-1")))
        await store.send(.open(.fleet, key("pull_request:github.com/o/r#1")))
        store.exhaustivity = .on

        await store.send(.viewsFeed(.stale(["node:a-1"])))
        await store.receive(\.inboxPath.element)
        await store.receive(\.tasksPath.element)
        XCTAssertEqual(store.state.inboxPath.count, 2, "the review is not sent the change, it observes its own key")
    }

    @MainActor
    func testWaitingForYouTakesItsStatusFromTheApp() async throws {
        let store = makeStore()
        let status = try Fixtures.document("home-ios").status
        await store.send(.triage(.feedStatus(status)))
        XCTAssertEqual(store.state.triage.status, status)
        XCTAssertEqual(store.state.triage.badge, 4)
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
