import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class SchedulesTests: XCTestCase {
    func testEachScheduleCarriesWhenItRunsNextAndWhatYouCanDoToIt() throws {
        let list = ScheduleList(document: try Samples.document("schedules-populated"))

        XCTAssertEqual(list.title, "Scheduled Tasks")
        XCTAssertEqual(list.entries.map(\.title), ["Morning digest", "Dependency audit", "Release checklist"])
        XCTAssertEqual(list.entries.map(\.frequency), ["Daily", "Weekly", "Once"])
        XCTAssertEqual(list.entries.first?.nextRun, Date(timeIntervalSince1970: 1_767_250_800))
        XCTAssertNil(list.entries.last?.nextRun, "a schedule that has run its one time has no next run")
        XCTAssertEqual(list.entries.first?.run?.id, "sched_run:s-a1@3")
        XCTAssertEqual(list.entries.first?.toggle?.label, "Pause")
        XCTAssertEqual(list.entries.first?.delete?.confirm?.button, "Delete")
        XCTAssertEqual(list.entries.first?.link, .view(key: "schedule:a1"))
    }

    func testAPausedScheduleIsToldByItsWarningBadgeAndOffersResume() throws {
        let list = ScheduleList(document: try Samples.document("schedules-populated"))

        XCTAssertEqual(list.entries.map(\.isPaused), [false, true, false])
        XCTAssertEqual(list.entries[1].toggle?.label, "Resume")
        XCTAssertEqual(list.entries[2].badges.map(\.text), ["Completed"])
    }

    func testTheAddButtonOpensTheNewScheduleForm() throws {
        let list = ScheduleList(document: try Samples.document("schedules-populated"))

        XCTAssertEqual(list.add, .open(ViewKey(rawValue: "schedule:new")))
    }

    func testNoSchedulesShowTheDaemonsEmptyNotice() throws {
        let list = ScheduleList(document: try Fixtures.document("schedules-ios"))

        XCTAssertTrue(list.entries.isEmpty)
        XCTAssertEqual(list.empty?.text, "No scheduled tasks")
        XCTAssertEqual(list.add, .open(ViewKey(rawValue: "schedule:new")))
    }

    @MainActor
    private func loaded(
        sent: SentActions = SentActions(),
        respond: @escaping @Sendable (ActionRequest) throws -> ActionResponse = { _ in try Responses.applied("Done") }
    ) async throws -> TestStoreOf<SchedulesFeature> {
        let document = try Samples.document("schedules-populated")
        let store = TestStore(initialState: SchedulesFeature.State()) {
            SchedulesFeature()
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
    func testRunNowSendsTheDaemonsActionAtOnce() async throws {
        let sent = SentActions()
        let store = try await loaded(sent: sent, respond: { _ in try Responses.applied("Started") })
        let run = try XCTUnwrap(store.state.list?.entries.first?.run)

        await store.send(.perform(run))
        await store.receive(\.delegate.completed, "Started")

        XCTAssertEqual(sent.requests.map(\.actionId), ["sched_run:s-a1@3"])
        XCTAssertEqual(sent.requests.first?.dataEpoch, "sample-epoch")
    }

    @MainActor
    func testDeletingAScheduleAsksFirst() async throws {
        let sent = SentActions()
        let store = try await loaded(sent: sent)
        let delete = try XCTUnwrap(store.state.list?.entries.first?.delete)

        await store.send(.perform(delete))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.message, "Future runs will stop. Delete this schedule?")
        XCTAssertTrue(sent.requests.isEmpty)

        await store.send(.actions(.confirm))
        await store.receive(\.delegate.completed)
        XCTAssertEqual(sent.requests.map(\.actionId), ["sched_delete:s-a1@3"])
    }

    @MainActor
    func testADeletedScheduleLeavesTheListAtOnceAndReturnsIfTheDeleteFails() async throws {
        let gate = Gate()
        let store = try await loaded()
        store.dependencies.viewsClient.perform = { _ in
            await gate.wait()
            throw URLError(.notConnectedToInternet)
        }
        let entry = try XCTUnwrap(store.state.list?.entries.first)

        await store.send(.perform(try XCTUnwrap(entry.delete)))
        await store.send(.actions(.confirm))
        await store.receive(\.actions.sent)
        XCTAssertFalse(store.state.list?.entries.contains { $0.id == entry.id } ?? true)

        await store.send(.screen(.loaded(try XCTUnwrap(store.state.screen.document))))
        XCTAssertFalse(store.state.list?.entries.contains { $0.id == entry.id } ?? true)

        await gate.open()
        await store.receive(\.actions.settled)
        XCTAssertEqual(store.state.list?.entries.first?.id, entry.id)
        await store.receive(\.delegate.failed)
    }

    @MainActor
    func testAFailedActionIsReportedAndLeavesTheListAlone() async throws {
        let store = try await loaded(respond: { _ in throw URLError(.notConnectedToInternet) })

        await store.send(.perform(try XCTUnwrap(store.state.list?.entries[1].toggle)))
        await store.receive(\.delegate.failed)

        XCTAssertEqual(store.state.list?.entries.count, 3)
        XCTAssertFalse(store.state.actions.inFlight)
    }

    @MainActor
    func testTappingAScheduleOrAddOpensTheEditor() async throws {
        let store = try await loaded()

        await store.send(.open(try XCTUnwrap(store.state.list?.entries.first?.link)))
        await store.receive(\.delegate.open, ViewKey(rawValue: "schedule:a1"))
        await store.send(.add)
        await store.receive(\.delegate.open, ViewKey(rawValue: "schedule:new"))
    }

    @MainActor
    func testAScheduleListTheDaemonNoLongerServesIsReportedMissing() async throws {
        let store = TestStore(initialState: SchedulesFeature.State()) {
            SchedulesFeature()
        } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { $0.finish(throwing: ViewsClientError.missingView) } }
        }
        store.exhaustivity = .off

        await store.send(.task)
        await store.receive(\.screen.missing)

        XCTAssertNil(store.state.list)
        XCTAssertEqual(store.state.screen.phase, .missing)
    }
}
