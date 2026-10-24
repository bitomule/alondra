import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class TasksFeatureTests: XCTestCase {
    @MainActor
    private func store(
        showing document: ViewDocument,
        sent: SentActions = SentActions(),
        respond: @escaping @Sendable (ActionRequest) throws -> ActionResponse = { _ in try Responses.applied("Completed cleared") },
        load: @escaping @Sendable (ViewKey, String?) async throws -> ViewFetch = { _, _ in throw ViewsClientError.missingView }
    ) -> TestStoreOf<TasksFeature> {
        TestStore(initialState: TasksFeature.State()) {
            TasksFeature()
        } withDependencies: {
            $0.viewsClient.observe = ViewsClient.showing(document)
            $0.viewsClient.load = load
            $0.viewsClient.perform = { request in sent.record(request); return try respond(request) }
            $0.uuid = .incrementing
        }
    }

    @MainActor
    private func loaded(_ document: ViewDocument, sent: SentActions = SentActions(),
                        respond: @escaping @Sendable (ActionRequest) throws -> ActionResponse = { _ in try Responses.applied("Completed cleared") },
                        load: @escaping @Sendable (ViewKey, String?) async throws -> ViewFetch = { _, _ in throw ViewsClientError.missingView }
    ) async -> TestStoreOf<TasksFeature> {
        let store = store(showing: document, sent: sent, respond: respond, load: load)
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()
        return store
    }

    @MainActor
    func testTheViewIsLoadedAndProjectedIntoTheBoard() async throws {
        let document = try Samples.document("tasks-populated")
        let store = store(showing: document)

        await store.send(.task)
        await store.receive(\.screen.task)
        await store.receive(\.screen.loaded) {
            $0.screen.document = document
            $0.screen.phase = .loaded
            $0.actions.dataEpoch = "sample-epoch"
        }
        await store.receive(\.screen.ended)
        await store.finish()

        XCTAssertEqual(store.state.board?.active.requests.map(\.id), ["r-5", "r-1", "r-2"])
    }

    @MainActor
    func testFoldsOpenAndCloseOneByOne() async throws {
        let store = await loaded(try Samples.document("tasks-populated"))

        await store.send(.foldToggled("finished")) { $0.expanded = ["finished"] }
        await store.send(.foldToggled("active")) { $0.expanded = ["finished", "active"] }
        await store.send(.foldToggled("finished")) { $0.expanded = ["active"] }
    }

    @MainActor
    func testAChainOpensAndClosesOnItsOwn() async throws {
        let store = await loaded(try Samples.document("tasks-populated"))

        await store.send(.chainToggled("chain:t-install-texts")) { $0.openChains = ["chain:t-install-texts"] }
        await store.send(.chainToggled("chain:t-install-texts")) { $0.openChains = [] }
    }

    @MainActor
    func testOpeningAFoldSentWithoutItsRowsAsksForThemOnce() async throws {
        let document = try Samples.document("tasks-populated") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            for index in sections.indices where sections[index]["id"] as? String == "finished" {
                sections[index]["items"] = [[String: Any]]()
                sections[index]["more"] = ["key": "tasks?section=finished&after=0", "remaining": 27]
            }
            object["sections"] = sections
        }
        let page = try Samples.document("tasks-populated")
        let asked = LockIsolated<[ViewKey]>([])
        let store = await loaded(document, load: { key, _ in
            asked.withValue { $0.append(key) }
            return .document(page)
        })
        XCTAssertEqual(store.state.board?.finished.requests.count, 0)
        XCTAssertEqual(store.state.board?.finished.count, 27)

        await store.send(.foldToggled("finished"))
        await store.receive(\.showMore)
        await store.receive(\.moreLoaded)

        XCTAssertEqual(asked.value, [ViewKey(rawValue: "tasks?section=finished&after=0")])
        XCTAssertEqual(store.state.board?.finished.requests.count, 2)
        XCTAssertEqual(store.state.board?.finished.more?.remaining, 7)

        await store.send(.foldToggled("finished"))
        await store.send(.foldToggled("finished"))
        await store.finish()
        XCTAssertEqual(asked.value.count, 1, "rows already fetched are shown again without asking")
    }

    @MainActor
    func testShowMoreLoadsTheNextPageOfThatSectionOnly() async throws {
        let document = try Samples.document("tasks-populated")
        let page = try Samples.document("tasks-populated") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            for index in sections.indices where sections[index]["id"] as? String == "finished" {
                sections[index]["items"] = [[
                    "type": "row", "id": "r-9", "icon": "mission", "title": "Older request", "depth": 0, "struck": false,
                    "subtitle": "Finished", "link": ["kind": "view", "key": "task:t-9"],
                ]]
                sections[index]["more"] = ["key": "tasks?section=finished&after=40", "remaining": 2]
            }
            object["sections"] = sections
        }
        let asked = LockIsolated<[ViewKey]>([])
        let store = await loaded(document, load: { key, _ in
            asked.withValue { $0.append(key) }
            return .document(page)
        })

        await store.send(.showMore("finished"))
        await store.receive(\.moreLoaded)

        XCTAssertEqual(asked.value, [ViewKey(rawValue: "tasks?section=finished&after=20")])
        XCTAssertEqual(store.state.board?.finished.requests.map(\.id), ["r-3", "r-4", "r-9"])
        XCTAssertEqual(store.state.board?.finished.more?.remaining, 2)
        XCTAssertEqual(store.state.board?.active.requests.count, 3)
        XCTAssertTrue(store.state.loadingMore.isEmpty)
    }

    @MainActor
    func testShowMoreIsAskedOnceWhileThePageIsOnItsWay() async throws {
        let page = try Samples.document("tasks-populated")
        let asked = LockIsolated(0)
        let gate = Gate()
        let store = await loaded(try Samples.document("tasks-populated"), load: { _, _ in
            asked.withValue { $0 += 1 }
            await gate.wait()
            return .document(page)
        })

        await store.send(.showMore("finished"))
        await store.send(.showMore("finished"))
        XCTAssertEqual(store.state.loadingMore, ["finished"])
        await gate.open()
        await store.receive(\.moreLoaded)
        await store.finish()

        XCTAssertEqual(asked.value, 1)
    }

    @MainActor
    func testAPageThatFailsToLoadSaysSoAndCanBeAskedAgain() async throws {
        let store = await loaded(try Samples.document("tasks-populated"), load: { _, _ in throw ViewsClientError.notInstalled })

        await store.send(.showMore("finished"))
        await store.receive(\.moreFailed)
        await store.receive(\.delegate.failed)
        XCTAssertTrue(store.state.loadingMore.isEmpty)
    }

    @MainActor
    func testAChangedFirstPageDropsThePagesLoadedAfterIt() async throws {
        let first = try Samples.document("tasks-populated")
        let page = try Samples.document("tasks-populated")
        let store = await loaded(first, load: { _, _ in .document(page) })
        await store.send(.showMore("finished"))
        await store.receive(\.moreLoaded)
        XCTAssertEqual(store.state.pages["finished"]?.rows.count, 2)

        let newer = try Samples.document("tasks-populated") { $0["version"] = "vtasks-newer" }
        await store.send(.screen(.loaded(newer)))

        XCTAssertTrue(store.state.pages.isEmpty, "the rows after the first twenty are asked for again, not guessed")
    }

    @MainActor
    func testTappingARowAsksTheAppToOpenItsScreen() async throws {
        let store = await loaded(try Samples.document("tasks-populated"))

        await store.send(.open(.view(key: "task:t-1")))
        await store.receive(\.delegate.open, ViewKey(rawValue: "task:t-1"))
        await store.send(.open(.url(url: "https://github.com/example/repo/pull/1")))
    }

    @MainActor
    func testClearingCompletedSendsTheDaemonsActionAndReportsItAsAToast() async throws {
        let sent = SentActions()
        let store = await loaded(try Samples.document("tasks-populated"), sent: sent)

        await store.send(.clearCompleted)
        await store.receive(\.delegate.completed, "Completed cleared")

        let request = try XCTUnwrap(sent.requests.first)
        XCTAssertEqual(request.actionId, "clear_completed:view@12")
        XCTAssertEqual(request.dataEpoch, "sample-epoch")
        XCTAssertEqual(request.client, .ios)
        XCTAssertNil(store.state.actions.action, "the finished action does not stay presented")
    }

    @MainActor
    func testARefusedClearIsReportedAsAFailure() async throws {
        let store = await loaded(try Samples.document("tasks-populated"), respond: { _ in
            try Responses.rejected("Nothing to clear")
        })

        await store.send(.clearCompleted)
        await store.receive(\.delegate.failed, "Nothing to clear")
    }

    @MainActor
    func testClearingEmptiesCompletedAtOnceAndBringsItBackIfTheDaemonRefuses() async throws {
        let gate = Gate()
        let document = try Samples.document("tasks-populated")
        let store = await loaded(document, respond: { _ in try Responses.rejected("Nothing to clear") })
        store.dependencies.viewsClient.perform = { _ in
            await gate.wait()
            return try Responses.rejected("Nothing to clear")
        }
        let before = try XCTUnwrap(store.state.board)
        XCTAssertGreaterThan(before.finished.count, 0)

        await store.send(.clearCompleted)
        await store.receive(\.actions.sent)
        XCTAssertEqual(store.state.board?.finished.count, 0, "Completed empties before the daemon answers")
        XCTAssertEqual(store.state.board?.finished.requests, [])
        XCTAssertNil(store.state.board?.finished.more)
        XCTAssertNil(store.state.board?.clearCompleted, "the button leaves with what it cleared")
        XCTAssertEqual(store.state.board?.active, before.active)

        await store.send(.screen(.loaded(document)))
        XCTAssertEqual(store.state.board?.finished.count, 0, "a stale board cannot bring it back")

        await gate.open()
        await store.receive(\.actions.settled)
        XCTAssertEqual(store.state.board, before)
        await store.receive(\.delegate.failed, "Nothing to clear")
    }

    @MainActor
    func testWithNothingToClearTheButtonDoesNothing() async throws {
        let sent = SentActions()
        let nothingToClear = try Samples.document("tasks-populated") { object in
            object["sections"] = (object["sections"] as? [[String: Any]] ?? []).map { section in
                var section = section
                section["actions"] = nil
                return section
            }
        }
        let store = await loaded(nothingToClear, sent: sent)

        await store.send(.clearCompleted)

        XCTAssertTrue(sent.requests.isEmpty)
    }

    @MainActor
    func testWithdrawingARequestAsksForTheDaemonsConfirmationFirst() async throws {
        let sent = SentActions()
        let document = try Samples.document("tasks-populated") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            var items = sections[1]["items"] as? [[String: Any]] ?? []
            items[2]["actions"] = [[
                "id": "withdraw:r-2@4", "label": "Withdraw Request", "style": "destructive", "presentation": "button",
                "input": ["kind": "none"], "attachments": false, "enabled": true,
                "confirm": ["title": "Withdraw Request", "message": "Withdraw this request?", "button": "Withdraw Request"],
            ]]
            sections[1]["items"] = items
            object["sections"] = sections
        }
        let store = await loaded(document, sent: sent)
        let withdraw = try XCTUnwrap(store.state.board?.active.requests.last?.withdraw)

        await store.send(.perform(withdraw))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.message, "Withdraw this request?")
        XCTAssertTrue(sent.requests.isEmpty, "nothing is sent before the person confirms")

        await store.send(.actions(.confirm))
        await store.receive(\.actions.response)
        XCTAssertEqual(sent.requests.map(\.actionId), ["withdraw:r-2@4"])
    }

    @MainActor
    func testPullToRefreshLoadsAFreshCopyAndAFailureIsReported() async throws {
        let document = try Samples.document("tasks-populated")
        let fresh = try Samples.document("tasks-populated") { $0["version"] = "vtasks-fresh" }
        let store = await loaded(document, load: { key, known in
            XCTAssertNil(known, "a refresh never asks for 'unchanged'")
            XCTAssertEqual(key, .tasks)
            return .document(fresh)
        })

        await store.send(.refresh)
        await store.receive(\.screen.loaded)
        XCTAssertEqual(store.state.screen.document?.version, "vtasks-fresh")

        let failing = await loaded(document, load: { _, _ in throw ViewsClientError.notInstalled })
        await failing.send(.refresh)
        await failing.receive(\.delegate.failed)
    }

    @MainActor
    func testAViewTheDaemonNoLongerServesIsReportedMissing() async throws {
        let store = TestStore(initialState: TasksFeature.State()) {
            TasksFeature()
        } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { $0.finish(throwing: ViewsClientError.missingView) } }
        }
        store.exhaustivity = .off

        await store.send(.task)
        await store.receive(\.screen.missing)

        XCTAssertEqual(store.state.screen.phase, .missing)
        XCTAssertNil(store.state.board)
    }
}
