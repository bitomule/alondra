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
            $0.scheduleOperations = ScheduleOperations(
                list: { await server.list() }, save: { _, _ in .confirmed },
                run: { _ in .confirmed }, delete: { await server.delete($0) },
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
        }
        await store.receive(\.deleted) {
            $0.busyIDs.remove(schedule.id); $0.inFlightTargets.remove(.schedule(schedule.id))
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
            $0.scheduleOperations = ScheduleOperations(
                list: { [schedule] },
                save: { _, _ in .confirmed },
                run: { _ in .rejected("launchRefused") },
                delete: { _ in .confirmed },
                projects: { [] }, options: { _ in [] }
            )
        }
        await store.send(.appeared) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false; $0.tasks = [schedule] }
        await store.send(.runTapped(schedule.id)) { $0.busyIDs.insert(schedule.id); $0.inFlightTargets.insert(.schedule(schedule.id)) }
        await store.receive(\.runFinished) {
            $0.busyIDs.remove(schedule.id); $0.inFlightTargets.remove(.schedule(schedule.id))
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
    func unknownScheduleRunBlocksRunAndDeleteUntilJournalReconciles() async throws {
        let schedule = try makeSchedule()
        let runID = UUID()
        let deleteID = UUID()
        var state = ScheduleFeature.State()
        state.tasks = [schedule]
        let store = TestStore(initialState: state) { ScheduleFeature() } withDependencies: {
            $0.scheduleOperations = ScheduleOperations(
                list: { [schedule] }, save: { _, _ in .confirmed },
                run: { _ in .unknown(runID) }, delete: { _ in .unknown(deleteID) },
                projects: { [] }, options: { _ in [] }
            )
        }
        await store.send(.runTapped(schedule.id)) { $0.busyIDs.insert(schedule.id); $0.inFlightTargets.insert(.schedule(schedule.id)) }
        await store.receive(\.runFinished) {
            $0.busyIDs.remove(schedule.id); $0.inFlightTargets.remove(.schedule(schedule.id))
            $0.uncertainByTarget[.schedule(schedule.id)] = runID
            $0.lastMutationOutcome = .unknown(runID)
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        await store.send(.runTapped(schedule.id))
        await store.send(.deleteConfirmed(schedule.id))
        await store.send(.commandReconciled(UUID()))
        await store.send(.commandReconciled(runID)) {
            $0.uncertainByTarget = [:]
            $0.lastMutationOutcome = nil
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        await store.send(.deleteConfirmed(schedule.id)) { $0.busyIDs.insert(schedule.id); $0.inFlightTargets.insert(.schedule(schedule.id)) }
        await store.receive(\.deleted) {
            $0.busyIDs.remove(schedule.id); $0.inFlightTargets.remove(.schedule(schedule.id))
            $0.uncertainByTarget[.schedule(schedule.id)] = deleteID
            $0.lastMutationOutcome = .unknown(deleteID)
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        await store.send(.deleteConfirmed(schedule.id))
        await store.finish()
    }

    @Test
    func unknownScheduleSaveStaysBlockedAfterEditorDismissal() async throws {
        let schedule = try makeSchedule()
        let commandID = UUID()
        var state = ScheduleFeature.State()
        state.tasks = [schedule]
        state.editingID = schedule.id
        state.draft = ScheduledTaskDraft(schedule)
        state.optionsReady = true
        let store = TestStore(initialState: state) { ScheduleFeature() } withDependencies: {
            $0.scheduleOperations = ScheduleOperations(
                list: { [schedule] }, save: { _, _ in .unknown(commandID) },
                run: { _ in .confirmed }, delete: { _ in .confirmed },
                projects: { [] }, options: { _ in [] }
            )
        }
        await store.send(.saveTapped) {
            $0.submittedDrafts[0] = ScheduledTaskDraft(schedule)
            $0.isSaving = true
            $0.inFlightTargets.insert(.schedule(schedule.id))
        }
        await store.receive(\.saved) {
            $0.isSaving = false
            $0.inFlightTargets.remove(.schedule(schedule.id))
            $0.uncertainByTarget[.schedule(schedule.id)] = commandID
            $0.uncertainDraftRevisions[commandID] = 0
            $0.lastMutationOutcome = .unknown(commandID)
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        await store.send(.saveTapped)
        await store.send(.editorDismissed) {
            $0.editorRevision = 1
            $0.editingID = nil
            $0.draft = nil
            $0.optionsReady = false
            $0.optionsRequest = 1
        }
        await store.send(.editTapped(schedule.id)) {
            $0.editorRevision = 2
            $0.editingID = schedule.id
            $0.draft = ScheduledTaskDraft(schedule)
            $0.optionsRequest = 2
            $0.optionsLoading = true
        }
        await store.receive(\.optionsLoaded) { $0.optionsLoading = false; $0.optionsReady = true }
        await store.send(.saveTapped)
        #expect(store.state.uncertainByTarget[.schedule(schedule.id)] == commandID)
        var nextDraft = ScheduledTaskDraft(schedule)
        nextDraft.title = "A new edit after reopening"
        await store.send(.draftChanged(nextDraft)) { $0.draft = nextDraft }
        await store.send(.commandResolved(commandID, rejection: nil)) {
            $0.uncertainByTarget = [:]
            $0.uncertainDraftRevisions = [:]
            $0.submittedDrafts = [:]
            $0.lastMutationOutcome = .confirmed
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        #expect(store.state.draft == nextDraft)
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
    func heldSaveBlocksAllMutationsOnScheduleAndUnknownNeedsExactResolution() async throws {
        let schedule = try makeSchedule()
        let commandID = UUID()
        let gate = MutationGate()
        var state = ScheduleFeature.State()
        state.tasks = [schedule]
        state.editingID = schedule.id
        state.draft = ScheduledTaskDraft(schedule)
        state.optionsReady = true
        let store = TestStore(initialState: state) { ScheduleFeature() } withDependencies: {
            $0.scheduleOperations = ScheduleOperations(
                list: { [schedule] }, save: { _, _ in await gate.wait() },
                run: { _ in .confirmed }, delete: { _ in .confirmed },
                projects: { [] }, options: { _ in [] }
            )
        }
        await store.send(.saveTapped) {
            $0.submittedDrafts[0] = ScheduledTaskDraft(schedule)
            $0.isSaving = true
            $0.inFlightTargets.insert(.schedule(schedule.id))
        }
        await store.send(.runTapped(schedule.id))
        await store.send(.enabledChanged(schedule.id, false))
        await store.send(.deleteConfirmed(schedule.id))
        await gate.release(.unknown(commandID))
        await store.receive(\.saved) {
            $0.isSaving = false
            $0.inFlightTargets.remove(.schedule(schedule.id))
            $0.uncertainByTarget[.schedule(schedule.id)] = commandID
            $0.uncertainDraftRevisions[commandID] = 0
            $0.lastMutationOutcome = .unknown(commandID)
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        await store.send(.runTapped(schedule.id))
        await store.send(.commandResolved(UUID(), rejection: nil))
        #expect(store.state.uncertainByTarget[.schedule(schedule.id)] == commandID)
        await store.send(.commandResolved(commandID, rejection: nil)) {
            $0.uncertainByTarget = [:]
            $0.uncertainDraftRevisions = [:]
            $0.submittedDrafts = [:]
            $0.draft = nil
            $0.editingID = nil
            $0.lastMutationOutcome = .confirmed
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
        await store.send(.runTapped(schedule.id)) {
            $0.busyIDs.insert(schedule.id)
            $0.inFlightTargets.insert(.schedule(schedule.id))
        }
        await store.receive(\.runFinished) {
            $0.busyIDs.remove(schedule.id)
            $0.inFlightTargets.remove(.schedule(schedule.id))
            $0.lastMutationOutcome = .confirmed
        }
        await store.receive(\.reconcile) { $0.isLoading = true }
        await store.receive(\.loaded) { $0.isLoading = false }
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
            $0.scheduleOperations = ScheduleOperations(
                list: { [a, b] },
                save: { _, _ in preconditionFailure("Save must wait for the active catalog") },
                run: { _ in .confirmed }, delete: { _ in .confirmed },
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
