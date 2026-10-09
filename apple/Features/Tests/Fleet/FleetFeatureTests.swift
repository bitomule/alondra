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
        XCTAssertEqual(overview.groups.map(\.title), ["Demo Garden", "Zeta", "Alpha", "Idle"])
        XCTAssertEqual(overview.groups.map(\.collapsed), [false, false, false, true], "idle nodes start collapsed, last")
        XCTAssertEqual(overview.groups.last?.count, 2)
        XCTAssertEqual(overview.groups.first?.count, 2)
        let node = try XCTUnwrap(overview.groups.first?.nodes.first)
        XCTAssertEqual(node.title, "Restore the native review experience")
        XCTAssertEqual(node.state?.text, "Waiting for an answer")
        XCTAssertEqual(node.state?.tone, .warning)
        XCTAssertEqual(node.subtitle, "Alondra · iOS")
        XCTAssertEqual(node.badges, [], "context and cost stay on the node's screen")
        XCTAssertEqual(overview.orchestrator?.state?.text, "Idle")
        XCTAssertEqual(node.pending, 2)
        XCTAssertEqual(node.activity, "Said: “The simulator pass completed successfully.”")
        XCTAssertEqual(node.activityAt, "2026-09-30T09:58:00Z")
        XCTAssertEqual(node.link, .view(key: "node:d887a34b-249e-4487-a89b-57de38c26372"))
        XCTAssertTrue(node.close?.id?.hasPrefix("close:") == true)
        XCTAssertNil(overview.orchestrator?.close)
    }

    func testReadsThePaceOfTheFleetApartFromTheNodeGroups() throws {
        let overview = FleetOverview(document: try Fixtures.document("fleet-pacing-ios-es"))

        let pacing = try XCTUnwrap(overview.pacing)
        XCTAssertEqual(pacing.summary, "Trabajando 6/6 · En cola 2 · En pausa 2 · Mac: memoria 12 % libre (justo)")
        XCTAssertEqual(pacing.limitText, "Límite: automático (6)")
        XCTAssertEqual(pacing.waiting.map(\.title), [
            "Fix the crash when the schedule list is empty", "Translate the onboarding into Spanish",
            "Build the importer", "Refresh the App Store screenshots",
        ])
        XCTAssertEqual(pacing.waiting.first?.state, "En cola · 1.º de 2 · el Mac va justo de memoria (12 % libre)")
        XCTAssertEqual(pacing.waiting.map { $0.action?.command }, ["pause", "pause", "resume", "resume"])
        XCTAssertEqual(pacing.waiting.map(\.paused), [false, false, true, true])
        XCTAssertFalse(overview.groups.contains { $0.id == "pacing" }, "the pace is not a group of nodes")
        let paused = overview.groups.flatMap(\.nodes).first { $0.id == "zeta-worker" }
        XCTAssertEqual(paused?.resume?.id?.hasPrefix("resume:zeta-worker@"), true)
        XCTAssertNil(paused?.pause)
    }

    func testResumingAPausedTaskShowsAtOnceAndARefusalBringsItBack() async throws {
        let gate = Gate()
        let store = gated(gate, reply: #"{"protocol":1,"outcome":"rejected","message":"Este nodo o esta tarea no está en pausa"}"#)
        await store.send(.screen(.loaded(try Fixtures.document("fleet-pacing-ios-es"))))
        let held = try XCTUnwrap(store.state.overview?.pacing?.waiting.last)
        let resume = try XCTUnwrap(held.action)

        await store.send(.perform(resume))
        await store.receive(\.actions.sent)
        let shown = try XCTUnwrap(store.state.overview?.pacing?.waiting.last)
        XCTAssertEqual(shown.state, "Reanudando…", "it says so before the daemon answers")
        XCTAssertNil(shown.action)
        XCTAssertFalse(shown.paused)

        await gate.open()
        await store.receive(\.actions.settled)
        XCTAssertEqual(store.state.overview?.pacing?.waiting.last, held, "a refused resume puts the row back")
    }

    func testTheFleetShowsTheLimitButLeavesChangingItToSettings() throws {
        let document = try Fixtures.document("fleet-pacing-ios-es")
        let actions = document.sections.flatMap(\.items).flatMap { item -> [WireAction] in
            switch item {
            case .actions(let bar): bar.actions
            case .row(let row): row.actions
            default: []
            }
        }
        XCTAssertFalse(actions.contains { $0.command == "set_limit" }, "Nodes at once lives in Settings")
        XCTAssertEqual(FleetOverview(document: document).pacing?.limitText, "Límite: automático (6)")
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
        let fleet = try Fixtures.document("fleet-ios")
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
        await store.send(.screen(.loaded(try Fixtures.document("fleet-ios"))))
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
        await store.send(.screen(.loaded(try Fixtures.document("fleet-ios"))))
        let close = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first?.close?.id)
        let request = ActionRequest(commandId: "c-1", dataEpoch: "", client: .ios, lang: "en", actionId: close)

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
        var items = try XCTUnwrap(sections[1]["items"] as? [[String: Any]])
        let interrupt: [String: Any] = [
            "id": "interrupt:d887a34b-249e-4487-a89b-57de38c26372@0", "label": "Interrupt", "style": "secondary",
            "presentation": "button", "input": ["kind": "none"], "attachments": false,
            "confirm": ["title": "Interrupt", "message": "Stop what this node is doing now?", "button": "Interrupt"], "enabled": true,
        ]
        items[0]["actions"] = [interrupt] + (items[0]["actions"] as? [[String: Any]] ?? [])
        sections[1]["items"] = items
        object["sections"] = sections
        let document = try JSONDecoder().decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
        await store.send(.screen(.loaded(document)))
        let node = try XCTUnwrap(store.state.overview?.groups.first?.nodes.first)
        let action = try XCTUnwrap(node.interrupt)
        XCTAssertEqual(node.close?.id?.hasPrefix("close:"), true)

        await store.send(.perform(action))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.title, "Interrupt")
        XCTAssertTrue(requests.value.isEmpty, "nothing is sent before the confirmation")

        await store.send(.actions(.confirm))
        await store.skipReceivedActions()
        XCTAssertEqual(requests.value.map(\.actionId), [action.id])
    }

    func testANodeThatIsNotWorkingOffersNoInterrupt() throws {
        let node = try XCTUnwrap(FleetOverview(document: try Fixtures.document("fleet-ios")).groups.first?.nodes.first)
        XCTAssertNil(node.interrupt)
    }

    func testARefusedCloseReportsTheReasonForTheAlert() async throws {
        let store = store(reply: #"{"protocol":1,"outcome":"rejected","message":"It is working"}"#)
        await store.send(.screen(.loaded(try Fixtures.document("fleet-ios"))))
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
