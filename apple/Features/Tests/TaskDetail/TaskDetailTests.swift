import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class TaskDetailTests: XCTestCase {
    func testEverySectionTheDaemonNamesLandsInItsPlace() throws {
        let detail = TaskDetail(document: try Samples.document("task-detail"))

        XCTAssertEqual(detail.title, "Restore the native review experience")
        XCTAssertEqual(detail.state, "Needs you")
        XCTAssertEqual(detail.stateTone, .warning)
        XCTAssertEqual(detail.since, Date(timeIntervalSince1970: 1_767_216_600))
        XCTAssertEqual(detail.node?.text, "Alondra · iOS")
        XCTAssertEqual(detail.node?.link, .view(key: "node:n-1"))
        XCTAssertEqual(detail.request?.text, "Polish the Alondra beta")
        XCTAssertNil(detail.request?.link)
        XCTAssertEqual(detail.now?.text, "question: Restore the native review experience")
        XCTAssertEqual(detail.now?.link, .view(key: "card:i-1"))
        XCTAssertEqual(detail.waiting.title, "Waiting for You")
        XCTAssertEqual(detail.waiting.elements.map(\.id), ["i-1", "i-2"])
        XCTAssertEqual(detail.requestText.elements.count, 1)
        XCTAssertEqual(detail.pullRequests.elements.map(\.id), ["github.com/example/repo#12"])
        XCTAssertEqual(detail.dependencies.elements.map(\.id), ["t-2", "t-3"])
        XCTAssertEqual(detail.documents.elements.map(\.id), ["rev-1"])
        XCTAssertEqual(detail.options.elements.map(\.label), ["PR handling", "Effort"])
        XCTAssertEqual(detail.history.elements.map(\.state), ["Working", "Needs you"])
        XCTAssertEqual(detail.history.elements.first?.summary, "Started")
    }

    func testTheStateIsTheHeaderFactWithoutALinkWhateverLanguageItIsLabelledIn() throws {
        let spanish = TaskDetail(document: try Samples.document("task-detail-es"))

        XCTAssertEqual(spanish.state, "Te necesita")
        XCTAssertEqual(spanish.node?.text, "Alondra · iOS")
        XCTAssertEqual(spanish.request?.text, "Pulir la beta de Alondra")
        XCTAssertEqual(spanish.waiting.title, "Te espera")
    }

    func testOnlyTheActionsTheTaskAllowsAreOffered() throws {
        let detail = TaskDetail(document: try Samples.document("task-detail"))

        XCTAssertEqual(detail.cancel?.enabled, true)
        XCTAssertNil(detail.reopen, "an older daemon still sends it switched off")
        XCTAssertNil(detail.archive, "an older daemon still sends it switched off")
    }

    func testATaskWhoseNodeTheUserClosedOffersRelaunchBeforeCancel() throws {
        let detail = TaskDetail(document: try Fixtures.document("task-closed-ios"))

        XCTAssertEqual(detail.state, "Node closed")
        XCTAssertEqual(detail.relaunch?.label, "Relaunch")
        XCTAssertEqual(detail.relaunch?.enabled, true)
        XCTAssertEqual(detail.actions.map(\.command), ["relaunch", "cancel"])
        XCTAssertNil(TaskDetail(document: try Fixtures.document("task-ios")).relaunch, "a task with a node offers none")
    }

    func testTheDaemonsOwnFixtureProjectsToo() throws {
        let detail = TaskDetail(document: try Fixtures.document("task-ios"))

        XCTAssertEqual(detail.state, "Needs you")
        XCTAssertEqual(detail.stateTone, .warning, "the daemon tints the state with the task's tone")
        XCTAssertNotNil(detail.since)
        XCTAssertEqual(detail.waiting.elements.count, 5)
        XCTAssertEqual(detail.pullRequests.elements.count, 1)
        XCTAssertEqual(detail.dependencies.elements.count, 0)
        XCTAssertNil(detail.node, "this task has no node yet")
        XCTAssertEqual(detail.actions.map(\.command), ["cancel"], "an open task can only be cancelled")
    }

    func testASectionTheScreenDoesNotKnowIsIgnored() throws {
        let document = try Samples.document("task-detail") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            sections.append(["id": "telemetry", "collapsed": false, "items": []])
            object["sections"] = sections
        }

        XCTAssertEqual(TaskDetail(document: document).title, "Restore the native review experience")
    }

    @MainActor
    private func loaded(
        sent: SentActions = SentActions(),
        respond: @escaping @Sendable (ActionRequest) throws -> ActionResponse = { _ in try Responses.applied("Cancelled") }
    ) async throws -> TestStoreOf<TaskDetailFeature> {
        let document = try Samples.document("task-detail")
        let store = TestStore(initialState: TaskDetailFeature.State(key: ViewKey(rawValue: "task:t-1"))) {
            TaskDetailFeature()
        } withDependencies: {
            $0.viewsClient.observe = ViewsClient.showing(document)
            $0.viewsClient.perform = { request in sent.record(request); return try respond(request) }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()
        return store
    }

    @MainActor
    func testCancellingAsksForConfirmationThenSendsTheDaemonsAction() async throws {
        let sent = SentActions()
        let store = try await loaded(sent: sent)
        let cancel = try XCTUnwrap(store.state.detail?.cancel)

        await store.send(.perform(cancel))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.title, "Cancel Task")
        XCTAssertTrue(sent.requests.isEmpty)

        await store.send(.actions(.confirm))
        await store.receive(\.delegate.completed, "Cancelled")
        XCTAssertEqual(sent.requests.map(\.actionId), ["cancel:t-1@4"])
        XCTAssertEqual(sent.requests.first?.dataEpoch, "sample-epoch")
    }

    @MainActor
    func testATappedActionLeavesTheBarAtOnceAndReturnsIfTheDaemonDoesNotApplyIt() async throws {
        let gate = Gate()
        let store = try await loaded()
        store.dependencies.viewsClient.perform = { _ in
            await gate.wait()
            return try Responses.rejected("The task already finished")
        }
        let cancel = try XCTUnwrap(store.state.detail?.cancel)

        await store.send(.perform(cancel))
        await store.send(.actions(.confirm))
        await store.receive(\.actions.sent)
        XCTAssertTrue(store.state.optimistic.hides(cancel.id))

        await store.send(.screen(.loaded(try XCTUnwrap(store.state.screen.document))))
        XCTAssertTrue(store.state.optimistic.hides(cancel.id), "a stale view does not show it again")

        await gate.open()
        await store.receive(\.actions.settled)
        XCTAssertFalse(store.state.optimistic.hides(cancel.id))
        await store.receive(\.delegate.failed, "The task already finished")
    }

    @MainActor
    func testDecliningTheConfirmationSendsNothing() async throws {
        let sent = SentActions()
        let store = try await loaded(sent: sent)

        await store.send(.perform(try XCTUnwrap(store.state.detail?.cancel)))
        await store.skipReceivedActions()
        await store.send(.actions(.cancelConfirmation))

        XCTAssertNil(store.state.actions.confirmation)
        XCTAssertTrue(sent.requests.isEmpty)
    }

    @MainActor
    func testAnActionThatIsNotAvailableAnyMoreIsReportedAndNotRetried() async throws {
        let store = try await loaded(respond: { _ in
            try ContractCodec.decode(
                ActionResponse.self,
                from: Data(#"{"protocol":1,"outcome":"no_longer_available","message":"The task already finished"}"#.utf8)
            )
        })

        await store.send(.perform(try XCTUnwrap(store.state.detail?.cancel)))
        await store.send(.actions(.confirm))
        await store.receive(\.delegate.failed, "The task already finished")
    }

    @MainActor
    func testAnActionThatChangedAsksBeforeSendingTheNewOne() async throws {
        let sent = SentActions()
        let store = try await loaded(sent: sent, respond: { request in
            if request.actionId == "cancel:t-1@4" {
                return try ContractCodec.decode(
                    ActionResponse.self,
                    from: Data(#"{"protocol":1,"outcome":"changed","message":"The task moved on","confirm":{"action_id":"cancel:t-1@5","button":"Cancel anyway"}}"#.utf8)
                )
            }
            return try Responses.applied("Cancelled")
        })

        await store.send(.perform(try XCTUnwrap(store.state.detail?.cancel)))
        await store.send(.actions(.confirm))
        await store.receive(\.actions.response)
        XCTAssertEqual(store.state.actions.changed?.button, "Cancel anyway")
        XCTAssertEqual(sent.requests.map(\.actionId), ["cancel:t-1@4"], "the new action waits for the person")

        await store.send(.actions(.sendChangedAction))
        await store.receive(\.delegate.completed, "Cancelled")
        XCTAssertEqual(sent.requests.map(\.actionId), ["cancel:t-1@4", "cancel:t-1@5"])
    }

    @MainActor
    func testEachLinkOnTheScreenOpensItsScreen() async throws {
        let store = try await loaded()
        let detail = try XCTUnwrap(store.state.detail)

        for link in [detail.node?.link, detail.waiting.elements.first?.link,
                     detail.pullRequests.elements.first?.link, detail.dependencies.elements.first?.link,
                     detail.documents.elements.first?.link] {
            let link = try XCTUnwrap(link)
            guard case .view(let key) = link else { return XCTFail("\(link) is not a view link") }
            await store.send(.open(link))
            await store.receive(\.delegate.open, ViewKey(rawValue: key))
        }
    }

    @MainActor
    func testATaskTheDaemonDroppedIsReportedMissing() async throws {
        let store = TestStore(initialState: TaskDetailFeature.State(key: ViewKey(rawValue: "task:t-gone"))) {
            TaskDetailFeature()
        } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { $0.finish(throwing: ViewsClientError.missingView) } }
        }
        store.exhaustivity = .off

        await store.send(.task)
        await store.receive(\.screen.missing)

        XCTAssertNil(store.state.detail)
        XCTAssertEqual(store.state.screen.phase, .missing)
    }
}
