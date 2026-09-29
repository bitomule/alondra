import AlondraContracts
import AlondraDomain
import AlondraFleetFeature
import ComposableArchitecture
import Foundation
import Testing

private func node(_ id: String, project: String = "Alondra", orchestrator: Bool = false, runID: String? = nil) throws -> Node {
    let runtime = runID.map { #","orchestrator_runtime":{"phase":"failed","can_retry":true,"can_cancel":true,"active_run_id":"\#($0)"}"# } ?? ""
    let json = #"{"session_id":"\#(id)","short_id":"\#(id)","is_orchestrator":\#(orchestrator),"has_plugin":true,"state":"working","state_since":1,"state_source":"hook","project":"\#(project)"\#(runtime)}"#
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try decoder.decode(Node.self, from: Data(json.utf8))
}

private func task(_ id: String, agent: String?) -> DomainV2.Task {
    .init(id: .init(rawValue: id), requestId: "r", agentId: agent.map { .init(rawValue: $0) }, title: "Task \(id)",
          state: .running, revision: 1, started: true, reviewPrs: false, finalPresentation: nil, prId: nil,
          dependencies: [], stack: nil, documents: [:], archived: false)
}

private func workspace(nodes: [Node], agents: [DomainV2.Agent] = [], tasks: [DomainV2.Task] = [],
                       inbox: [DomainV2.InboxItem] = [], revision: UInt64 = 1) -> DomainWorkspace {
    let projection = DomainV2.DomainProjection(
        protocolVersion: 2, dataEpoch: "epoch", revision: revision, inbox: inbox,
        counts: .init(roots: UInt64(inbox.count), pending: 0, unreadInformation: 0), tasks: tasks, agents: agents,
        requests: [.init(id: "r", body: "Work", closed: false)], interactions: [], pullRequests: [], documents: [],
        documentRevisions: [], documentReviews: [], mergeAuthorizations: [], stackReadiness: [])
    return DomainWorkspace(projection, runtime: .init(nodes: nodes))
}

private func root(_ id: String, agent: String) -> DomainV2.InboxItem {
    .init(id: id, kind: "information", subject: .interaction(interactionId: .init(rawValue: id)), title: id, body: nil,
          taskId: nil, requestId: nil, agentId: .init(rawValue: agent), revision: 1, order: 1, pending: true, children: [],
          availableActions: [])
}

private actor Calls {
    var commandIDs: [String] = []
    var messages: [String] = []
    func record(_ id: String, _ message: String = "") { commandIDs.append(id); messages.append(message) }
}

private func operations(_ calls: Calls, outcome: NodeMutationOutcome) -> NodeOperations {
    NodeOperations(send: { _, message, id in await calls.record(id, message.text); return outcome },
                   close: { _, id in await calls.record(id); return outcome },
                   threads: { _ in [] },
                   retryOrchestrator: { run, id in await calls.record(id, run); return outcome })
}

@MainActor
struct FleetFeatureTests {
    @Test
    func anAgentKeepsAllItsTasksAndCountsOnlyVisiblePendingCards() throws {
        let current = workspace(nodes: [try node("a")], agents: [.init(id: "b", name: "Beta", connected: true)],
                                tasks: [task("1", agent: "a"), task("2", agent: "a"), task("3", agent: "b")],
                                inbox: [root("interaction:x", agent: "a"), root("interaction:y", agent: "a")])
        let snapshot = ProjectFleetUseCase().execute(current, visibleInboxIDs: ["interaction:x"], sequence: 1)
        let a = try #require(snapshot.node("a"))
        #expect(a.tasks.map(\.id) == ["1", "2"])
        #expect(a.pendingCount == 1)
        let b = try #require(snapshot.node("b"), "An Agent without telemetry is still listed")
        #expect(b.node.displayName == "Beta")
        #expect(b.tasks.map(\.id) == ["3"])
        #expect(ProjectFleetUseCase().search(snapshot, query: "Task 2").flatMap(\.nodes).map(\.id) == ["a"])
    }

    @Test
    func closingAgainAfterAnUnknownOutcomeReusesTheCommandID() async throws {
        let calls = Calls()
        let snapshot = ProjectFleetUseCase().execute(workspace(nodes: [try node("a")]), visibleInboxIDs: [], sequence: 1)
        var state = FleetFeature.State()
        state.snapshot = snapshot
        let store = TestStore(initialState: state) { FleetFeature() } withDependencies: {
            $0.nodeOperations = operations(calls, outcome: .unknown)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        for _ in 0..<2 {
            await store.send(.closeRequested("a"))
            await store.send(.closeConfirmed)
            await store.receive(\.closeFinished)
        }
        let ids = await calls.commandIDs
        #expect(ids.count == 2)
        #expect(ids[0] == ids[1])
    }

    @Test
    func sendingAgainReusesTheCommandIDUntilTheMessageChanges() async throws {
        let calls = Calls()
        let snapshot = ProjectFleetUseCase().execute(workspace(nodes: [try node("a")]), visibleInboxIDs: [], sequence: 1)
        let store = TestStore(initialState: NodeDetailFeature.State(sessionID: "a")) { NodeDetailFeature() } withDependencies: {
            $0.nodeOperations = operations(calls, outcome: .unknown)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.snapshotChanged(snapshot))
        await store.send(.messageChanged("Status?"))
        await store.send(.sendTapped)
        await store.receive(\.sendFinished)
        #expect(store.state.message == "Status?", "An unknown outcome keeps the draft")
        await store.send(.sendTapped)
        await store.receive(\.sendFinished)
        await store.send(.messageChanged("Status now?"))
        await store.send(.sendTapped)
        await store.receive(\.sendFinished)
        let ids = await calls.commandIDs
        #expect(ids[0] == ids[1])
        #expect(ids[2] != ids[0])
    }

    @Test
    func retryingTheOrchestratorNamesItsRun() async throws {
        let calls = Calls()
        let snapshot = ProjectFleetUseCase().execute(workspace(nodes: [try node("o", orchestrator: true, runID: "run-1")]),
                                                     visibleInboxIDs: [], sequence: 1)
        let store = TestStore(initialState: NodeDetailFeature.State(sessionID: "o")) { NodeDetailFeature() } withDependencies: {
            $0.nodeOperations = operations(calls, outcome: .confirmed)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.snapshotChanged(snapshot))
        await store.send(.retryRequested)
        await store.send(.retryConfirmed)
        await store.receive(\.retryFinished)
        #expect(await calls.messages == ["run-1"])
    }
}
