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
        await store.send(.open(.newRequest, key("request:r-1")))

        XCTAssertEqual(store.state.tasksPath.map(\.key), [key("task:t-1")])
        XCTAssertEqual(store.state.fleetPath.map(\.key), [key("node:a-1")])
        XCTAssertEqual(store.state.newRequestPath.map(\.key), [key("request:r-1")])
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
        await store.send(.tasks(.action(.delegate(.failed("Couldn’t send. Try again.")))))
        XCTAssertEqual(store.state.notice?.event, .failure("Couldn’t send. Try again."))
        await clock.advance(by: .seconds(3))
        XCTAssertNotNil(store.state.notice)

        await store.send(.fleet(.action(.delegate(.completed("Done")))))
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
            await store.send(.open(tab, key("task:t-1")))
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
    func testTheReviewReportsItsActionsAsToasts() async throws {
        let store = makeStore()
        await store.send(.open(.inbox, key("card:i-9")))
        let id = try XCTUnwrap(store.state.inboxPath.ids.first)
        await store.send(.inboxPath(.element(id: id, action: .review(.actions(.delegate(.completed("Accepted")))))))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("Accepted"))
    }

    // MARK: Close refusal

    @MainActor
    func testARefusedCloseFromTheFleetOpensTheRefusalAlert() async throws {
        var state = AppFeature.State()
        state.fleet.action.action = try ContractCodec.decode(
            AlondraContract.Action.self,
            from: Data(#"{"id":"close:n-1@1","label":"Close Node","style":"destructive","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#.utf8)
        )
        let store = makeStore(state)
        let refused = try ContractCodec.decode(
            ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"rejected","message":"It is working"}"#.utf8)
        )

        await store.send(.fleet(.action(.response(refused))))
        XCTAssertEqual(store.state.closeRefusal, "It is working")
        await store.send(.dismissCloseRefusal)
        XCTAssertNil(store.state.closeRefusal)
    }

    // MARK: Feed

    @MainActor
    func testAChangeReachesTheRootScreensAndOnlyTheScreensStillOnTheLegacyRenderer() async {
        let store = makeStore()
        await store.send(.open(.tasks, key("task:t-1")))
        await store.send(.open(.inbox, key("node:a-1")))
        await store.send(.open(.inbox, key("card:i-9")))

        store.exhaustivity = .on
        await store.send(.viewsFeed(.stale(["task:t-1"])))
        await store.receive(\.tasks.stale)
        await store.receive(\.fleet.stale)
        await store.receive(\.newRequest.stale)
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
}
