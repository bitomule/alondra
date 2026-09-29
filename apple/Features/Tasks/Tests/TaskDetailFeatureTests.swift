import AlondraContracts
import AlondraDomain
import AlondraTasksFeature
import ComposableArchitecture
import Foundation
import Testing

private func workspace(_ name: String) throws -> DomainWorkspace {
    let env = ProcessInfo.processInfo.environment
    let url: URL
    if let runfiles = env["TEST_SRCDIR"] {
        url = URL(fileURLWithPath: runfiles).appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("apple/APIClient/Tests/domain-v2-wire-fixtures.json")
    } else {
        url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("APIClient/Tests/domain-v2-wire-fixtures.json")
    }
    let all = try JSONDecoder().decode([String: DomainV2.DomainProjection].self, from: Data(contentsOf: url))
    return DomainWorkspace(try #require(all[name]))
}

private func action(_ kind: String, enabled: Bool = true) -> DomainV2.AvailableAction {
    .init(kind: kind, enabled: enabled, disabledReason: enabled ? nil : "task_not_active", expectedRevision: 9,
          presentationId: nil, expectedHead: nil)
}

private actor Recorder {
    var intents: [DomainIntent] = []
    var actions: [[DomainV2.AvailableAction]] = []
    func record(_ intent: DomainIntent, _ actions: [DomainV2.AvailableAction]) { intents.append(intent); self.actions.append(actions) }
}

private func client(_ recorder: Recorder, actions: [DomainV2.AvailableAction]) -> DomainWorkspaceClient {
    var client = DomainWorkspaceClient.inert
    client.taskActions = { _, _ in actions }
    client.perform = { intent, _, actions, _ in
        await recorder.record(intent, actions)
        return .recorded(.init(revision: 10, changed: true, effects: [], createdIds: []))
    }
    return client
}

@MainActor
struct TaskDetailFeatureTests {
    @Test
    func taskActionsComeFromTheServerAndOnlyOfferedOnesAreSent() async throws {
        let recorder = Recorder()
        let current = try workspace("completed")
        let offered = [action("cancel_task", enabled: false), action("reopen_task"), action("archive_task")]
        let store = TestStore(initialState: TaskDetailFeature.State(taskID: "t")) { TaskDetailFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, actions: offered)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.taskChanged(DomainTaskDetail("t", in: current), current))
        await store.receive(\.actionsLoaded)
        await store.send(.cancelTapped)
        #expect(store.state.confirmation == nil, "Cancel is not offered for a finished task")
        await store.send(.reopenTapped)
        await store.receive(\.mutationFinished)
        #expect(await recorder.intents == [.reopenTask("t")])
        #expect(await recorder.actions.first == offered)
        #expect(store.state.task?.state == .done, "The projection, not the command, changes the task state")
    }

    @Test
    func archivingTheFinishedTasksSendsOneArchivePerTask() async throws {
        let recorder = Recorder()
        let current = try workspace("completed")
        let store = TestStore(initialState: TaskBoardFeature.State()) { TaskBoardFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, actions: [action("archive_task")])
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.boardChanged(TaskBoardProjection(board: TaskBoard(current), workspace: current, hasSnapshot: true)))
        #expect(store.state.canArchive)
        await store.send(.archiveTapped)
        await store.receive(\.archiveFinished)
        #expect(await recorder.intents == [.archiveTask("t", archived: true)])
        #expect(store.state.archiveOutcome == .confirmed)
    }
}
