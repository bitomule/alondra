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
    func detailDoesNotAssumeServerTransitionAfterSuccessfulReopen() async throws {
        let task = try makeTask(state: "done")
        let store = TestStore(initialState: TaskDetailFeature.State(taskID: task.id)) { TaskDetailFeature() } withDependencies: {
            $0.taskOperations = TaskOperations(
                cancel: { _ in .rejected("refused") }, reopen: { _ in .confirmed }, archiveCompleted: { .obsolete }, details: { _ in nil }
            )
        }
        await store.send(.taskChanged(task)) { $0.task = task }
        await store.receive(\.detailLoaded)
        await store.send(.reopenTapped) { $0.isActing = true }
        await store.receive(\.mutationFinished) {
            $0.isActing = false
            $0.mutationOutcome = .confirmed
            $0.successfulActionCount = 1
        }
        #expect(store.state.task?.state == .done)
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
    func unrelatedHistoryCannotResolveUncertainReopen() async throws {
        let task = try makeTask(state: "done")
        let unrelatedUpdate = try makeTask(state: "done", stateSince: 200)
        let commandID = UUID()
        let store = TestStore(initialState: TaskDetailFeature.State(taskID: task.id)) { TaskDetailFeature() } withDependencies: {
            $0.taskOperations = TaskOperations(
                cancel: { _ in .rejected("refused") }, reopen: { _ in .unknown(commandID) }, archiveCompleted: { .obsolete }, details: { _ in nil }
            )
        }
        await store.send(.taskChanged(task)) { $0.task = task }
        await store.receive(\.detailLoaded)
        await store.send(.reopenTapped) { $0.isActing = true }
        await store.receive(\.mutationFinished) {
            $0.isActing = false
            $0.mutationOutcome = .unknown(commandID)
        }
        await store.send(.reopenTapped)
        #expect(store.state.mutationOutcome == .unknown(commandID))
        #expect(store.state.successfulActionCount == 0)
        await store.send(.taskChanged(unrelatedUpdate)) { $0.task = unrelatedUpdate }
        await store.receive(\.detailLoaded)
        await store.send(.reopenTapped)
        #expect(store.state.mutationOutcome == .unknown(commandID))
        await store.send(.commandReconciled(UUID()))
        #expect(store.state.mutationOutcome == .unknown(commandID))
        await store.send(.commandReconciled(commandID)) { $0.mutationOutcome = nil }
        await store.finish()
    }

    @Test
    func archiveUnknownRequiresItsExactJournalResolution() async {
        let commandID = UUID()
        var state = TaskBoardFeature.State()
        state.canArchive = true
        let store = TestStore(initialState: state) { TaskBoardFeature() } withDependencies: {
            $0.taskOperations = TaskOperations(
                cancel: { _ in .obsolete }, reopen: { _ in .obsolete },
                archiveCompleted: { .unknown(commandID) }, details: { _ in nil }
            )
        }
        await store.send(.archiveTapped) { $0.isArchiving = true }
        await store.receive(\.archiveFinished) {
            $0.isArchiving = false
            $0.archiveOutcome = .unknown(commandID)
        }
        await store.send(.boardChanged(TaskBoardProjection(board: .empty, canArchive: true, hasSnapshot: true))) {
            $0.hasSnapshot = true
        }
        await store.send(.archiveTapped)
        await store.send(.commandReconciled(UUID()))
        #expect(store.state.archiveOutcome == .unknown(commandID))
        await store.send(.commandReconciled(commandID)) { $0.archiveOutcome = nil }
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
    func missingTaskClearsPreviouslyLoadedDetail() async throws {
        let task = try makeTask(state: "running")
        let full = try makeFullTask()
        let store = TestStore(initialState: TaskDetailFeature.State(taskID: task.id)) { TaskDetailFeature() } withDependencies: {
            $0.taskOperations = TaskOperations(
                cancel: { _ in .obsolete }, reopen: { _ in .obsolete }, archiveCompleted: { .obsolete }, details: { _ in full }
            )
        }
        await store.send(.taskChanged(task)) { $0.task = task }
        await store.receive(\.detailLoaded) { $0.detail = full }
        await store.send(.taskChanged(nil)) { $0.task = nil; $0.detail = nil }
        #expect(store.state.detail == nil)
        await store.finish()
    }

    @Test
    func newHistoryKeyDropsOldDetailWhileNewDetailsLoad() async throws {
        let previous = try makeTask(state: "running")
        let updated = try makeTask(state: "done", stateSince: 200)
        let full = try makeFullTask()
        let store = TestStore(initialState: TaskDetailFeature.State(taskID: previous.id)) { TaskDetailFeature() } withDependencies: {
            $0.taskOperations = TaskOperations(
                cancel: { _ in .obsolete }, reopen: { _ in .obsolete }, archiveCompleted: { .obsolete },
                details: { task in task.historyKey == 100 ? full : nil }
            )
        }
        await store.send(.taskChanged(previous)) { $0.task = previous }
        await store.receive(\.detailLoaded) { $0.detail = full }
        await store.send(.taskChanged(updated)) { $0.task = updated; $0.detail = nil }
        await store.receive(\.detailLoaded)
        #expect(store.state.detail == nil)
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

    private func makeTask(state: String, stateSince: Int = 100) throws -> WorkTask {
        let data = Data(#"{"id":"t1","request":"r1","title":"Do work","dependsOn":[],"prs":[],"documents":[],"notices":[],"state":"\#(state)","stateSince":\#(stateSince),"createdAt":100}"#.utf8)
        return try JSONDecoder().decode(WorkTask.self, from: data)
    }

    private func makeFullTask() throws -> TaskInFull {
        let data = Data(#"{"task":{"id":"t1","request":"r1","title":"Do work","dependsOn":[],"prs":[],"documents":[],"notices":[],"state":"running","stateSince":100,"createdAt":100}}"#.utf8)
        return try JSONDecoder().decode(TaskInFull.self, from: data)
    }

    private func makeSchedule(id: String = "s1", project: String? = nil) throws -> ScheduledTask {
        let data = Data(#"{"id":"\#(id)","title":"Daily","text":"Do it","project":"\#(project ?? "A")","options":{},"disallowed_tools":[],"frequency":"daily","hour":9,"minute":0,"start_at":1790321400000,"enabled":true,"status":"never_run","revision":1}"#.utf8)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(ScheduledTask.self, from: data)
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
