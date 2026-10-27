import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class TaskDetailTests: XCTestCase {
    func testModelSelectionNavigationRemainsAvailableWithANonMutatingViewLink() throws {
        let document = try Samples.document("task-detail", editing: { object in
            var sections = object["sections"] as! [[String: Any]]
            let index = sections.firstIndex { $0["id"] as? String == "actions" }!
            sections[index]["items"] = [["type": "actions", "actions": [[
                "id": "", "label": "Change model", "style": "plain", "presentation": "button", "input": ["kind": "none"],
                "attachments": false, "enabled": true, "link": ["kind": "view", "key": "task_selection:t-1"]
            ]]]]
            object["sections"] = sections
        })
        let detail = TaskDetail(document: document)
        XCTAssertEqual(detail.modelSelection?.id, "")
        XCTAssertEqual(detail.modelSelection?.link, .view(key: "task_selection:t-1"))
    }

    func testEverySectionTheDaemonNamesLandsInItsPlace() throws {
        let detail = TaskDetail(document: try Samples.document("task-detail"))

        XCTAssertEqual(detail.title, "Restore the native review experience")
        XCTAssertEqual(detail.state, "Needs you")
        XCTAssertEqual(detail.stateTone, .warning)
        XCTAssertEqual(detail.since, Date(timeIntervalSince1970: 1_767_216_600))
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
        XCTAssertEqual(spanish.request?.text, "Pulir la beta de Alondra")
        XCTAssertEqual(spanish.waiting.title, "Te espera")
    }

    func testOnlyTheActionsTheTaskAllowsAreOffered() throws {
        let detail = TaskDetail(document: try Samples.document("task-detail"))

        XCTAssertEqual(detail.cancel?.enabled, true)
        XCTAssertNil(detail.reopen, "an older daemon still sends it switched off")
    }

    func testATaskWhoseNodeTheUserClosedOffersRelaunchBeforeCancel() throws {
        let detail = TaskDetail(document: try Fixtures.document("task-closed-ios"))

        XCTAssertEqual(detail.state, "Stopped · You closed its session before it delivered")
        XCTAssertEqual(detail.relaunch?.label, "Relaunch")
        XCTAssertEqual(detail.relaunch?.enabled, true)
        XCTAssertEqual(detail.actions.map(\.command), ["relaunch", "set_priority", "cancel"])
        XCTAssertNil(TaskDetail(document: try Fixtures.document("task-ios")).relaunch, "a task with a node offers none")
    }

    func testTheDaemonsOwnFixtureProjectsToo() throws {
        let detail = TaskDetail(document: try Fixtures.document("task-ios"))

        XCTAssertEqual(detail.state, "Needs you · 5 open questions")
        XCTAssertEqual(detail.stateTone, .warning, "the daemon tints the state with the task's tone")
        XCTAssertNotNil(detail.since)
        XCTAssertEqual(detail.waiting.elements.count, 5)
        XCTAssertEqual(detail.pullRequests.elements.count, 1)
        XCTAssertEqual(detail.dependencies.elements.count, 0)
        XCTAssertNil(detail.folder, "this task has no node yet")
        XCTAssertTrue(detail.readings.isEmpty)
        XCTAssertNil(detail.interrupt)
        XCTAssertEqual(detail.actions.map(\.command), ["set_priority", "cancel"], "an open task can only change its priority or be cancelled")
        XCTAssertEqual(detail.priority?.options.map(\.id), ["high", "normal", "low"])
    }

    func testWhatTheNodeReportsLandsInItsPlace() throws {
        let detail = TaskDetail(document: try Samples.document("task-detail-working"))

        XCTAssertEqual(detail.state, "Working")
        XCTAssertEqual(detail.request?.text, "Build a feature")
        XCTAssertEqual(detail.folder, "/work/alondra/.claude/worktrees/t-1")
        XCTAssertEqual(detail.readings.map(\.label), ["Running", "In the background", "Context", "Cost"])
        XCTAssertEqual(detail.readings.map(\.value), ["Bash: cargo test", "2 subagents · 1 shell", "84% of 200K tokens", "$1.20"])
        XCTAssertEqual(detail.activity.title, "Right now")
        XCTAssertEqual(detail.activity.elements.map(\.title), ["Editing views/tasks.rs"])
        XCTAssertEqual(detail.needs.elements.map(\.text), ["The staging key"])
        XCTAssertEqual(detail.steps.elements.count, 6, "the progress line and five steps")
        guard case .facts(let progress)? = detail.steps.elements.first else { return XCTFail("the steps open with their progress") }
        XCTAssertEqual(progress.map(\.value), ["2 of 5", "Running the tests"])
        XCTAssertEqual(detail.subagents.elements.count, 2)
        XCTAssertEqual(detail.workflows.title, "Workflows")
        XCTAssertEqual(detail.workflows.elements.count, 5)
        XCTAssertEqual(detail.background.elements.map(\.title), ["npm run dev", "swift build"])
        XCTAssertEqual(detail.background.elements.first?.detail, "Shell")
        XCTAssertEqual(detail.background.elements.first?.since, "2026-09-30T09:40:00Z")
        XCTAssertEqual(detail.background.elements.map { $0.state?.text }, ["Running", "Failed"])
        XCTAssertEqual(detail.background.elements.map { $0.state?.tone }, [.info, .critical])
        XCTAssertNil(detail.background.elements.last?.since, "an ended task says how long it ran, not since when")
        XCTAssertEqual(detail.lastMessage?.markdown.text, "Tests pass; opening the PR")
        XCTAssertEqual(detail.lastMessage?.at, "2026-09-30T09:58:00Z")
        XCTAssertEqual(detail.interrupt?.label, "Interrupt")
        XCTAssertEqual(detail.message?.label, "Message")
    }

    func testTheDaemonsWorkingTaskOffersInterruptAndAMessage() throws {
        let detail = TaskDetail(document: try Fixtures.document("task-working-ios"))

        XCTAssertEqual(detail.interrupt?.confirm?.button, "Interrupt")
        XCTAssertNotNil(detail.message)
        XCTAssertEqual(detail.folder, "/work/alondra/.claude/worktrees/t-1")
        XCTAssertFalse(detail.readings.isEmpty)
        XCTAssertFalse(detail.activity.elements.isEmpty)
        XCTAssertNotNil(detail.lastMessage)
    }

    func testWhatTheTaskAllowsLandsInItsPlaceWithItsRevoke() throws {
        let detail = TaskDetail(document: try Fixtures.document("task-allowed-ios"))

        XCTAssertEqual(detail.allowed.title, "Allowed in this task")
        XCTAssertEqual(detail.allowed.elements.map(\.command), ["git branch -D scratch/empty-state", "bazelisk test …"])
        XCTAssertEqual(detail.allowed.elements.first?.detail, "exact command, from the task's worktree · used 2 times")
        XCTAssertEqual(detail.allowed.elements.map { $0.revoke?.label }, ["Revoke", "Revoke"])
        XCTAssertEqual(detail.allowed.elements.map { $0.revoke?.command }, ["revoke_grant", "revoke_grant"])
        XCTAssertNotEqual(detail.allowed.elements[0].revoke?.id, detail.allowed.elements[1].revoke?.id, "each row revokes its own grant")

        let spanish = TaskDetail(document: try Fixtures.document("task-allowed-ios-es"))
        XCTAssertEqual(spanish.allowed.title, "Permitido en esta tarea")
        XCTAssertEqual(spanish.allowed.elements.map { $0.revoke?.label }, ["Revocar", "Revocar"])
        XCTAssertTrue(TaskDetail(document: try Fixtures.document("task-ios")).allowed.elements.isEmpty, "a task that allows nothing has no section")
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
    func testARevokedGrantLeavesAtOnceAndReturnsWithTheErrorIfTheDaemonRefuses() async throws {
        let gate = Gate()
        let sent = SentActions()
        let document = try Fixtures.document("task-allowed-ios")
        let store = TestStore(initialState: TaskDetailFeature.State(key: ViewKey(rawValue: document.key))) {
            TaskDetailFeature()
        } withDependencies: {
            $0.viewsClient.observe = ViewsClient.showing(document)
            $0.viewsClient.perform = { request in
                sent.record(request)
                await gate.wait()
                return try Responses.rejected("The task already ended")
            }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()
        let revoke = try XCTUnwrap(store.state.detail?.allowed.elements.first?.revoke)

        await store.send(.perform(revoke))
        await store.receive(\.actions.sent)
        XCTAssertTrue(store.state.optimistic.hides(revoke.id))
        XCTAssertEqual(sent.requests.map(\.actionId), [revoke.id])

        await gate.open()
        await store.receive(\.actions.settled)
        XCTAssertFalse(store.state.optimistic.hides(revoke.id))
        await store.receive(\.delegate.failed, "The task already ended")
    }

    @MainActor
    func testChoosingAPrioritySendsItsLevelAndShowsAtOnce() async throws {
        let sent = SentActions()
        let document = try Fixtures.document("task-pacing-ios-es")
        let store = TestStore(initialState: TaskDetailFeature.State(key: ViewKey(rawValue: document.key))) {
            TaskDetailFeature()
        } withDependencies: {
            $0.viewsClient.observe = ViewsClient.showing(document)
            $0.viewsClient.perform = { request in sent.record(request); return try Responses.applied("Prioridad cambiada") }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()
        let priority = try XCTUnwrap(store.state.detail?.priority)
        XCTAssertEqual(priority.options.map(\.label), ["Alta · ahora", "Normal", "Baja"])
        XCTAssertEqual(store.state.detail?.options.elements.first?.value, "Alta")
        XCTAssertEqual(store.state.detail?.pause?.label, "Pausar", "a queued task can be taken out of the queue")
        let low = try XCTUnwrap(priority.options.last)

        await store.send(.choose(priority, low))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.choices[priority.id ?? ""], "Baja")
        XCTAssertTrue(store.state.optimistic.hides(priority.id), "the new level shows before the daemon's view")
        XCTAssertEqual(sent.requests.map(\.actionId), [priority.id])
        XCTAssertEqual(sent.requests.map(\.optionId), ["low"])
    }

    @MainActor
    private func working(sent: SentActions) async throws -> TestStoreOf<TaskDetailFeature> {
        let document = try Fixtures.document("task-working-ios")
        let store = TestStore(initialState: TaskDetailFeature.State(key: ViewKey(rawValue: document.key))) {
            TaskDetailFeature()
        } withDependencies: {
            $0.viewsClient.observe = ViewsClient.showing(document)
            $0.viewsClient.perform = { request in sent.record(request); return try Responses.applied("Done") }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()
        return store
    }

    @MainActor
    func testInterruptingAsksForConfirmationThenStopsTheNodeAndLeavesTheBar() async throws {
        let sent = SentActions()
        let store = try await working(sent: sent)
        let interrupt = try XCTUnwrap(store.state.detail?.interrupt)

        await store.send(.perform(interrupt))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.button, "Interrupt")
        XCTAssertTrue(sent.requests.isEmpty)

        await store.send(.actions(.confirm))
        await store.skipReceivedActions()
        XCTAssertEqual(sent.requests.map(\.actionId), ["interrupt:a-1@0"])
    }

    @MainActor
    func testAMessageIsWrittenToTheTasksNodeAndTheComposerEmpties() async throws {
        let sent = SentActions()
        let store = try await working(sent: sent)
        XCTAssertEqual(store.state.composer.action?.id, "msg:a-1@0~t-1", "the composer is ready as soon as the task loads")
        XCTAssertFalse(store.state.canSend, "an empty message is not sent")

        await store.send(.messageChanged("Use the staging key"))
        XCTAssertTrue(store.state.canSend)
        await store.send(.send)
        await store.skipReceivedActions()

        XCTAssertEqual(sent.requests.map(\.actionId), ["msg:a-1@0~t-1"])
        XCTAssertEqual(sent.requests.first?.input.text, "Use the staging key")
        XCTAssertEqual(store.state.message, "")
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

        for link in [detail.waiting.elements.first?.link,
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
