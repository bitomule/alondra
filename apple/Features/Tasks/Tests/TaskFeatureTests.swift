import AlondraContracts
import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct TaskFeatureTests {
    @Test
    func scheduleInspectorTracksSelectionAndConfirmedDeletionThroughReload() async throws {
        let schedule = try makeSchedule()
        let server = ScheduleListFixture([schedule])
        let store = TestStore(initialState: ScheduleFeature.State()) { ScheduleFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.scheduleOperations = ScheduleOperations(
                list: { await server.list() }, save: { _, _, _ in .confirmed },
                run: { _, _ in .confirmed }, delete: { id, _ in await server.delete(id) },
                projects: { [] }, options: { _ in [] }
            )
        }
        await store.send(.appeared) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false; $0.tasks = [schedule] }
        await store.send(.selected(schedule.id)) { $0.selectedID = schedule.id; $0.inspectorVisible = true }
        await store.send(.inspectorPresented(false)) { $0.inspectorVisible = false }
        // Re-selecting the same row or refreshing must respect the user's closed inspector.
        await store.send(.selected(schedule.id))
        await store.send(.refresh) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        await store.send(.inspectorPresented(true)) { $0.inspectorVisible = true }
        await store.send(.deleteConfirmed(schedule.id)) {
            $0.busyIDs.insert(schedule.id); $0.inFlightTargets.insert(.schedule(schedule.id))
            $0.attempts[.schedule(schedule.id)] = ScheduleAttempt(id: UUID(0).uuidString, operation: .delete)
        }
        await store.receive(\.deleted) {
            $0.busyIDs.remove(schedule.id); $0.inFlightTargets.remove(.schedule(schedule.id))
            $0.attempts = [:]
            $0.lastMutationOutcome = .confirmed
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) {
            $0.isLoading = false; $0.tasks = []; $0.selectedID = nil; $0.inspectorVisible = false
        }
        #expect(await server.deletedIDs() == [schedule.id])
        // A delayed List binding cannot reopen a deleted selection.
        await store.send(.selected(schedule.id))
        await store.send(.inspectorPresented(true))
        await store.finish()
    }

    @Test
    func scheduleRunFailureStaysVisibleThroughReconciliationThenClearsOnManualRefresh() async throws {
        let schedule = try makeSchedule()
        let store = TestStore(initialState: ScheduleFeature.State()) { ScheduleFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.scheduleOperations = ScheduleOperations(
                list: { [schedule] },
                save: { _, _, _ in .confirmed },
                run: { _, _ in .rejected("launchRefused") },
                delete: { _, _ in .confirmed },
                projects: { [] }, options: { _ in [] }
            )
        }
        await store.send(.appeared) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false; $0.tasks = [schedule] }
        await store.send(.runTapped(schedule.id)) {
            $0.busyIDs.insert(schedule.id); $0.inFlightTargets.insert(.schedule(schedule.id))
            $0.attempts[.schedule(schedule.id)] = ScheduleAttempt(id: UUID(0).uuidString, operation: .run)
        }
        await store.receive(\.runFinished) {
            $0.busyIDs.remove(schedule.id); $0.inFlightTargets.remove(.schedule(schedule.id))
            $0.attempts = [:]
            $0.mutationError = "launchRefused"
            $0.lastMutationOutcome = .rejected("launchRefused")
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        #expect(store.state.error == "launchRefused")
        #expect(store.state.tasks[0].lastRequest == nil)
        await store.send(.refresh) { $0.isLoading = true; $0.mutationError = nil }
        await store.receive(\.loaded) { $0.isLoading = false }
        #expect(store.state.error == nil)
        await store.finish()
    }

    @Test
    func switchingEditorsClearsOldCatalogAndCannotSaveBeforeNewOptionsLoad() async throws {
        let a = try makeSchedule(id: "a", project: "A")
        let b = try makeSchedule(id: "b", project: "B")
        let spec = try JSONDecoder().decode(OptionSpec.self, from: Data(#"{"id":"mode","label":"Mode","kind":"choice","values":[{"id":"fast","label":"Fast"}],"default":"fast"}"#.utf8))
        let gate = OptionGate()
        var state = ScheduleFeature.State()
        state.tasks = [a, b]
        let store = TestStore(initialState: state) { ScheduleFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.scheduleOperations = ScheduleOperations(
                list: { [a, b] },
                save: { _, _, _ in preconditionFailure("Save must wait for the active catalog") },
                run: { _, _ in .confirmed }, delete: { _, _ in .confirmed },
                projects: { ["A", "B"] },
                options: { project in project == "A" ? [spec] : await gate.wait() }
            )
        }
        await store.send(.editTapped("a")) {
            $0.editorRevision = 1
            $0.editingID = "a"
            $0.draft = ScheduledTaskDraft(a)
            $0.optionsRequest = 1
            $0.optionsLoading = true
        }
        await store.receive(\.optionsLoaded) {
            $0.optionsLoading = false
            $0.optionsReady = true
            $0.projects = ["A", "B"]
            $0.optionSpecs = [spec]
        }
        await store.send(.editTapped("b")) {
            $0.editorRevision = 2
            $0.editingID = "b"
            $0.draft = ScheduledTaskDraft(b)
            $0.projects = []
            $0.optionSpecs = []
            $0.optionsReady = false
            $0.optionsLoading = true
            $0.optionsRequest = 2
        }
        await store.send(.saveTapped)
        await gate.release([])
        await store.receive(\.optionsLoaded) {
            $0.optionsLoading = false
            $0.optionsReady = true
            $0.projects = ["A", "B"]
        }
        await store.send(.editorDismissed) {
            $0.editorRevision = 3
            $0.editingID = nil
            $0.draft = nil
            $0.optionsReady = false
            $0.optionsRequest = 3
        }
        await store.finish()
    }

    @Test
    func anUnknownRunIsRetriedWithTheSameAttemptID() async throws {
        let schedule = try makeSchedule()
        let attempts = AttemptLog()
        var state = ScheduleFeature.State()
        state.tasks = [schedule]
        let store = TestStore(initialState: state) { ScheduleFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.scheduleOperations = ScheduleOperations(
                list: { [schedule] }, save: { _, _, _ in .confirmed },
                run: { _, attempt in await attempts.record(attempt); return .unknown },
                delete: { _, attempt in await attempts.record(attempt); return .confirmed },
                projects: { [] }, options: { _ in [] })
        }
        store.exhaustivity = .off
        await store.send(.runTapped(schedule.id))
        await store.receive(\.runFinished)
        #expect(store.state.mutationError != nil)
        await store.send(.runTapped(schedule.id))
        await store.receive(\.runFinished)
        await store.send(.deleteConfirmed(schedule.id))
        await store.receive(\.deleted)
        let ids = await attempts.ids
        #expect(ids[0] == ids[1], "Running again after an unknown outcome reuses its attempt")
        #expect(ids[2] != ids[0], "Deleting is a different operation")
    }

    private func makeSchedule(id: String = "s1", project: String? = nil) throws -> ScheduledTask {
        let data = Data(#"{"id":"\#(id)","title":"Daily","text":"Do it","project":"\#(project ?? "A")","options":{},"disallowed_tools":[],"frequency":"daily","hour":9,"minute":0,"weekday":null,"interval":null,"start_at":1790321400000,"enabled":true,"next_run":null,"status":"never_run","last_run":null,"last_request":null,"last_error":null,"last_run_id":null,"revision":1}"#.utf8)
        return ScheduledTask(try DomainV2.decode(DomainV2.Schedule.self, from: data))
    }
}

private actor OptionGate {
    private var continuation: CheckedContinuation<[OptionSpec], Never>?
    private var value: [OptionSpec]?

    func wait() async -> [OptionSpec] {
        if let value { return value }
        return await withCheckedContinuation { continuation = $0 }
    }

    func release(_ value: [OptionSpec]) {
        if let continuation {
            continuation.resume(returning: value)
            self.continuation = nil
        } else {
            self.value = value
        }
    }
}

private actor MutationGate {
    private var continuation: CheckedContinuation<TaskMutationOutcome, Never>?
    private var value: TaskMutationOutcome?

    func wait() async -> TaskMutationOutcome {
        if let value { return value }
        return await withCheckedContinuation { continuation = $0 }
    }

    func release(_ value: TaskMutationOutcome) {
        if let continuation {
            continuation.resume(returning: value)
            self.continuation = nil
        } else {
            self.value = value
        }
    }
}

private enum TestFailure: Error, LocalizedError {
    case launchRefused
    var errorDescription: String? { "launchRefused" }
}

private actor ScheduleListFixture {
    private var tasks: [ScheduledTask]
    private var deleted: [String] = []
    init(_ tasks: [ScheduledTask]) { self.tasks = tasks }
    func list() -> [ScheduledTask] { tasks }
    func deletedIDs() -> [String] { deleted }
    func delete(_ id: String) -> TaskMutationOutcome {
        deleted.append(id)
        tasks.removeAll { $0.id == id }
        return .confirmed
    }
}

private actor AttemptLog {
    var ids: [String] = []
    func record(_ id: String) { ids.append(id) }
}
