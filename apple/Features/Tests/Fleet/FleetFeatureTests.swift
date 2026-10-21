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

    func testReadsEachNodeAsItsTaskAndItsStateByProject() throws {
        let overview = FleetOverview(document: try Fixtures.document("fleet-ios"))

        XCTAssertEqual(overview.groups.map(\.title), ["Demo Garden", "Zeta", "Alpha"], "nodes whose sessions closed are not listed")
        XCTAssertEqual(overview.groups.map(\.collapsed), [false, false, false])
        XCTAssertEqual(overview.groups.first?.count, 2)
        let node = try XCTUnwrap(overview.groups.first?.nodes.first)
        XCTAssertEqual(node.title, "Restore the native review experience")
        XCTAssertEqual(node.state?.text, "Waiting for an answer")
        XCTAssertEqual(node.state?.tone, .warning)
        XCTAssertNil(node.subtitle)
        XCTAssertEqual(node.badges, [], "context, cost and the model stay on the node's screen")
        XCTAssertEqual(node.pending, 0)
        XCTAssertNil(node.activity, "what it does or said is on the node's screen")
        XCTAssertEqual(node.link, .view(key: "node:d887a34b-249e-4487-a89b-57de38c26372"))
        XCTAssertNil(node.close, "a node ends with its task")
    }

    /// Fleet as a Mac with an earlier daemon sends it, whose first node still offers Close Node.
    private func fleetWithClose() throws -> ViewDocument {
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(Fixtures.document("fleet-ios"))) as? [String: Any])
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        var items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
        let close: [String: Any] = [
            "id": "close:d887a34b-249e-4487-a89b-57de38c26372@6", "label": "Close Node", "style": "destructive",
            "presentation": "button", "input": ["kind": "none"], "attachments": false,
            "confirm": ["title": "Close Node", "message": "Close this node?", "button": "Close Node"], "enabled": true,
        ]
        items[0]["actions"] = (items[0]["actions"] as? [[String: Any]] ?? []) + [close]
        sections[0]["items"] = items
        object["sections"] = sections
        return try JSONDecoder().decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    func testTheFleetListsOnlyNodesWithoutThePaceOrTheLimit() throws {
        let document = try Fixtures.document("fleet-ios-es")
        let actions = document.sections.flatMap(\.items).flatMap { item -> [WireAction] in
            switch item {
            case .actions(let bar): bar.actions
            case .row(let row): row.actions
            default: []
            }
        }
        XCTAssertFalse(actions.contains { $0.command == "set_limit" }, "Nodes at once lives in Settings")
        XCTAssertFalse(document.sections.contains { $0.id == "pacing" }, "waiting tasks show in Tasks, not in Fleet")
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

    func testClosingANodeOfAnEarlierDaemonAsksForConfirmationAndThenSendsTheCloseAction() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try fleetWithClose())))
        let close = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first?.close)

        await store.send(.perform(close))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.title, "Close Node")
        XCTAssertTrue(requests.value.isEmpty, "nothing is sent before the confirmation")

        await store.send(.actions(.confirm))
        await store.receive(\.delegate.screen.completed, "Closed")

        XCTAssertEqual(requests.value.map(\.actionId), [close.id])
    }

    private func gated(_ gate: Gate, reply: String) -> TestStoreOf<FleetFeature> {
        let store = TestStore(initialState: FleetFeature.State()) { FleetFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { _ in
                await gate.wait()
                return try Fixtures.response(reply)
            }
        }
        store.exhaustivity = .off
        return store
    }

    func testAClosedNodeLeavesTheListAtOnceAndAStaleListCannotBringItBack() async throws {
        let gate = Gate()
        let store = gated(gate, reply: #"{"protocol":1,"outcome":"applied","message":"Closed"}"#)
        let fleet = try fleetWithClose()
        await store.send(.screen(.loaded(fleet)))
        let node = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first)

        await store.send(.perform(try XCTUnwrap(node.close)))
        await store.send(.actions(.confirm))
        await store.receive(\.actions.sent)
        XCTAssertNil(store.state.overview?.groups.first { $0.nodes.contains { $0.id == node.id } },
                     "the node is gone before the daemon answers")

        await store.send(.screen(.loaded(fleet)))
        XCTAssertNil(store.state.overview?.groups.first { $0.nodes.contains { $0.id == node.id } })

        await gate.open()
        await store.receive(\.actions.settled)
        await store.send(.screen(.loaded(fleet)))
        XCTAssertNil(store.state.overview?.groups.first { $0.nodes.contains { $0.id == node.id } },
                     "the view the daemon applied it over still cannot")
    }

    func testAFailedCloseBringsTheNodeBack() async throws {
        let gate = Gate()
        let store = gated(gate, reply: #"{"protocol":1,"outcome":"rejected","message":"It is working"}"#)
        await store.send(.screen(.loaded(try fleetWithClose())))
        let node = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first)

        await store.send(.perform(try XCTUnwrap(node.close)))
        await store.send(.actions(.confirm))
        await store.receive(\.actions.sent)
        XCTAssertNil(store.state.overview?.groups.first?.nodes.first { $0.id == node.id })

        await gate.open()
        await store.receive(\.actions.settled)
        XCTAssertEqual(store.state.overview?.groups.first?.nodes.first?.id, node.id)
        await store.receive(\.delegate.closeRefused, "It is working")
    }

    func testANodeClosedFromItsScreenLeavesTheListUntilTheAppSettlesIt() async throws {
        let store = store()
        await store.send(.screen(.loaded(try fleetWithClose())))
        let close = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first?.close?.id)
        let request = ActionRequest(commandId: "c-1", dataEpoch: try XCTUnwrap(store.state.screen.document).dataEpoch, client: .ios, lang: "en", actionId: close)

        await store.send(.handedOff(request))
        XCTAssertFalse(store.state.overview?.groups.flatMap(\.nodes).contains { $0.close?.id == close } == true)
        await store.send(.handOffSettled(request, applied: false))
        XCTAssertEqual(store.state.overview?.groups.first?.nodes.first?.close?.id, close)
    }

    func testInterruptingAWorkingNodeConfirmsThenSendsTheDaemonsAction() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(Fixtures.document("fleet-ios"))) as? [String: Any])
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        var items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
        let interrupt: [String: Any] = [
            "id": "interrupt:d887a34b-249e-4487-a89b-57de38c26372@0", "label": "Interrupt", "style": "secondary",
            "presentation": "button", "input": ["kind": "none"], "attachments": false,
            "confirm": ["title": "Interrupt", "message": "Stop what this node is doing now?", "button": "Interrupt"], "enabled": true,
        ]
        items[0]["actions"] = [interrupt] + (items[0]["actions"] as? [[String: Any]] ?? [])
        sections[0]["items"] = items
        object["sections"] = sections
        let document = try JSONDecoder().decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
        await store.send(.screen(.loaded(document)))
        let node = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first)
        let action = try XCTUnwrap(node.interrupt)
        XCTAssertNil(node.close)

        await store.send(.perform(action))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.title, "Interrupt")
        XCTAssertTrue(requests.value.isEmpty, "nothing is sent before the confirmation")

        await store.send(.actions(.confirm))
        await store.skipReceivedActions()
        XCTAssertEqual(requests.value.map(\.actionId), [action.id])
    }

    func testAForceStopTakesTheStopSlotAndLeavesItAtOnceWhenSent() async throws {
        let store = store()
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(Fixtures.document("fleet-ios"))) as? [String: Any])
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        var items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
        let forceStop: [String: Any] = [
            "id": "force_stop:d887a34b-249e-4487-a89b-57de38c26372@0", "label": "Force stop", "style": "destructive",
            "presentation": "button", "input": ["kind": "none"], "attachments": false,
            "confirm": ["title": "Force stop", "message": "End its process now?", "button": "Force stop"], "enabled": true, "stamp": "Stopping…",
        ]
        items[0]["actions"] = [forceStop] + (items[0]["actions"] as? [[String: Any]] ?? [])
        sections[0]["items"] = items
        object["sections"] = sections
        let document = try JSONDecoder().decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
        await store.send(.screen(.loaded(document)))
        let action = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first?.interrupt)
        XCTAssertEqual(action.label, "Force stop")
        let request = ActionRequest(commandId: "c-1", dataEpoch: document.dataEpoch, client: .ios, lang: "en", actionId: try XCTUnwrap(action.id))

        await store.send(.handedOff(request))
        XCTAssertNil(store.state.overview?.groups.first?.nodes.first?.interrupt, "it stops offering to stop as soon as it is sent")
        XCTAssertEqual(store.state.overview?.groups.first?.nodes.first?.stamp, "Stopping…", "and says it is stopping at once")
        await store.send(.handOffSettled(request, applied: false))
        XCTAssertEqual(store.state.overview?.groups.first?.nodes.first?.interrupt?.id, action.id)
    }

    func testANodeThatIsNotWorkingOffersNoInterrupt() throws {
        let node = try XCTUnwrap(FleetOverview(document: try Fixtures.document("fleet-ios")).groups.first?.nodes.first)
        XCTAssertNil(node.interrupt)
    }

    func testARefusedCloseReportsTheReasonForTheAlert() async throws {
        let store = store(reply: #"{"protocol":1,"outcome":"rejected","message":"It is working"}"#)
        await store.send(.screen(.loaded(try fleetWithClose())))
        let close = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first?.close)

        await store.send(.perform(close))
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

    private func observingStore(
        load: @escaping @Sendable (ViewKey, String?) async throws -> ViewFetch
    ) throws -> TestStoreOf<FleetFeature> {
        let first = try Fixtures.document("fleet-ios")
        let open = LockIsolated<[AsyncThrowingStream<ViewFetch, any Error>.Continuation]>([])
        let store = TestStore(initialState: FleetFeature.State()) { FleetFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = { _ in
                AsyncThrowingStream { continuation in
                    open.withValue { $0.append(continuation) }
                    continuation.yield(.document(first))
                }
            }
            $0.viewsClient.load = load
        }
        store.exhaustivity = .off
        return store
    }

    func testPullToRefreshFinishesWhileTheFleetStaysObserved() async throws {
        let fresh = try Fixtures.document("fleet-ios") { $0["version"] = "vfleet-fresh" }
        let loads = LockIsolated<[ViewKey]>([])
        let store = try observingStore { key, known in
            XCTAssertNil(known, "a refresh never asks for 'unchanged'")
            loads.withValue { $0.append(key) }
            return .document(fresh)
        }
        await store.send(.task)
        await store.receive(\.screen.loaded)

        let refresh = await store.send(.refresh)
        await refresh.finish(timeout: .seconds(2))

        await store.receive(\.screen.loaded)
        XCTAssertEqual(store.state.screen.document?.version, "vfleet-fresh")
        XCTAssertEqual(loads.value, [.fleet])
        await store.send(.screen(.stop))
    }

    func testPullToRefreshAlsoReloadsAnOpenSearch() async throws {
        let fleet = try Fixtures.document("fleet-ios") { $0["version"] = "vfleet-fresh" }
        let search = try Fixtures.document("fleet-ios") { $0["key"] = "search?q=ios"; $0["version"] = "vsearch-fresh" }
        let loads = LockIsolated<[ViewKey]>([])
        let store = try observingStore { key, _ in
            loads.withValue { $0.append(key) }
            return .document(key == .fleet ? fleet : search)
        }
        await store.send(.queryChanged("ios"))
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchStarted)

        let refresh = await store.send(.refresh)
        await refresh.finish(timeout: .seconds(2))

        await store.receive(\.search.loaded)
        XCTAssertEqual(loads.value, [.fleet, .search("ios")])
        XCTAssertEqual(store.state.search?.document?.version, "vsearch-fresh")
        await store.send(.search(.stop))
    }

    func testAFailedPullToRefreshFinishesAndIsReported() async throws {
        let store = try observingStore { _, _ in throw ViewsClientError.notInstalled }
        await store.send(.task)
        await store.receive(\.screen.loaded)

        let refresh = await store.send(.refresh)
        await refresh.finish(timeout: .seconds(2))

        await store.receive(\.delegate.screen.failed)
        XCTAssertEqual(store.state.screen.phase, .loaded, "the saved fleet stays on screen")
        await store.send(.screen(.stop))
    }
}
