import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class FleetFeatureTests: XCTestCase {
    private let clock = TestClock()

    private func store(
        requests: LockIsolated<[ActionRequest]> = LockIsolated([]),
        reply: String = #"{"protocol":1,"outcome":"applied","message":"Closed"}"#,
        observed: LockIsolated<[ViewKey]> = LockIsolated([])
    ) -> TestStoreOf<FleetFeature> {
        let store = TestStore(initialState: FleetFeature.State()) { FleetFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.uuid = .incrementing
            $0.viewsClient.observe = { key in
                observed.withValue { $0.append(key) }
                return AsyncThrowingStream { $0.finish() }
            }
            $0.viewsClient.perform = { request in
                requests.withValue { $0.append(request) }
                return try Fixtures.response(reply)
            }
        }
        store.exhaustivity = .off
        return store
    }

    func testReadsTheOrchestratorAndTheNodesByProject() throws {
        let overview = FleetOverview(document: try Fixtures.document("fleet-ios"))

        XCTAssertEqual(overview.orchestrator?.title, "Orchestrator")
        XCTAssertEqual(overview.orchestrator?.isOrchestrator, true)
        XCTAssertEqual(overview.groups.map(\.title), ["Demo Garden"])
        XCTAssertEqual(overview.groups.first?.count, 1)
        let node = try XCTUnwrap(overview.groups.first?.nodes.first)
        XCTAssertEqual(node.title, "Alondra · iOS")
        XCTAssertEqual(node.state, "Connected")
        XCTAssertEqual(node.pending, 1)
        XCTAssertEqual(node.lastMessage?.title, "The simulator pass completed successfully.")
        XCTAssertEqual(node.link, .view(key: "node:d887a34b-249e-4487-a89b-57de38c26372"))
        XCTAssertTrue(node.close?.id?.hasPrefix("close:") == true)
        XCTAssertNil(overview.orchestrator?.close)
    }

    func testObservesTheFleetWhenTheTabAppears() async throws {
        let observed = LockIsolated<[ViewKey]>([])
        let store = store(observed: observed)

        await store.send(.task)
        await store.receive(\.screen.task)
        await store.finish()

        XCTAssertEqual(observed.value, [.fleet])
    }

    func testSearchWaitsForTheTypingToPauseAndThenAsksTheDaemon() async {
        let observed = LockIsolated<[ViewKey]>([])
        let store = store(observed: observed)

        await store.send(.queryChanged("ios"))
        XCTAssertNil(store.state.search)
        await clock.advance(by: .milliseconds(249))
        XCTAssertNil(store.state.search)
        await clock.advance(by: .milliseconds(1))
        await store.receive(\.searchStarted, "ios")
        await store.receive(\.search.task)
        await store.finish()

        XCTAssertEqual(store.state.search?.key, .search("ios"))
        XCTAssertEqual(observed.value, [.search("ios")])
    }

    func testKeepsTypingRestartsTheWaitAndAsksOnlyForTheFinalQuery() async {
        let observed = LockIsolated<[ViewKey]>([])
        let store = store(observed: observed)

        await store.send(.queryChanged("i"))
        await clock.advance(by: .milliseconds(200))
        await store.send(.queryChanged("io"))
        await clock.advance(by: .milliseconds(200))
        XCTAssertNil(store.state.search)
        await clock.advance(by: .milliseconds(50))
        await store.receive(\.searchStarted, "io")
        await store.receive(\.search.task)
        await store.finish()

        XCTAssertEqual(observed.value, [.search("io")])
    }

    func testClearingTheQueryLeavesSearchAndNeverAsks() async {
        let observed = LockIsolated<[ViewKey]>([])
        let store = store(observed: observed)
        await store.send(.queryChanged("ios"))
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchStarted)
        await store.receive(\.search.task)

        await store.send(.queryChanged(""))
        XCTAssertNil(store.state.search)
        XCTAssertFalse(store.state.isSearching)

        await store.send(.queryChanged("a"))
        await store.send(.queryChanged("  "))
        await clock.advance(by: .seconds(1))
        XCTAssertEqual(observed.value, [.search("ios")])
    }

    func testADebounceForAQueryYouHaveMovedPastDoesNotStartASearch() async {
        let store = store()
        await store.send(.queryChanged("io"))

        await store.send(.searchStarted("ios"))

        XCTAssertNil(store.state.search)
        XCTAssertEqual(store.state.query, "io")
    }

    func testTheSearchIsTrimmedBeforeItIsAsked() async {
        let store = store()
        await store.send(.queryChanged(" ios "))

        await store.send(.searchStarted(" ios "))

        XCTAssertEqual(store.state.search?.key, .search("ios"))
    }

    func testClosingANodeAsksForConfirmationAndThenSendsTheCloseAction() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try Fixtures.document("fleet-ios"))))
        let close = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first?.close)

        await store.send(.close(close))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.title, "Close Node")
        XCTAssertTrue(requests.value.isEmpty, "nothing is sent before the confirmation")

        await store.send(.actions(.confirm))
        await store.receive(\.delegate.screen.completed, "Closed")

        XCTAssertEqual(requests.value.map(\.actionId), [close.id])
    }

    func testARefusedCloseReportsTheReasonForTheAlert() async throws {
        let store = store(reply: #"{"protocol":1,"outcome":"rejected","message":"It is working"}"#)
        await store.send(.screen(.loaded(try Fixtures.document("fleet-ios"))))
        let close = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first?.close)

        await store.send(.close(close))
        await store.send(.actions(.confirm))
        await store.receive(\.delegate.closeRefused, "It is working")
    }

    func testARefusalThatIsNotACloseIsNotReportedAsOne() async throws {
        let delegated = LockIsolated<[FleetFeature.Action.Delegate]>([])
        let store = TestStore(initialState: FleetFeature.State()) {
            CombineReducers {
                FleetFeature()
                Reduce<FleetFeature.State, FleetFeature.Action> { _, action in
                    if case .delegate(let value) = action { delegated.withValue { $0.append(value) } }
                    return .none
                }
            }
        } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { _ in try Fixtures.response(#"{"protocol":1,"outcome":"rejected","message":"No"}"#) }
        }
        store.exhaustivity = .off
        let other = try Fixtures.action(
            #"{"id":"msg:n-1@1","label":"Message","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#)

        await store.send(.actions(.present(other)))
        await store.send(.actions(.submit))
        await store.skipReceivedActions()

        XCTAssertEqual(delegated.value, [.screen(.failed("No"))])
    }

    func testTappingANodeOpensItsScreen() async {
        let store = store()

        await store.send(.follow(.view(key: "node:n-1")))
        await store.receive(\.delegate.screen.open, ViewKey(rawValue: "node:n-1"))
        await store.send(.follow(.url(url: "https://example.com")))
    }

    func testPullingToRefreshObservesAgainOnlyWhatIsOpen() async throws {
        let observed = LockIsolated<[ViewKey]>([])
        let store = store(observed: observed)

        await store.send(.refresh)
        await store.skipReceivedActions()
        await store.finish()

        XCTAssertEqual(observed.value, [.fleet])
    }
}
