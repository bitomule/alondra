import AlondraFleetFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct FleetFeatureTests {
    private let machine = #"{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}"#

    private func overview(tick: UInt64 = 1, nodes: String, tasks: String = "[]", questions: String = "[]") throws -> Overview {
        let json = #"{"tick":\#(tick),"nodes":\#(nodes),"open_questions":\#(questions),"news":[],"orchestrator_messages":[],"machine":\#(machine),"tasks":\#(tasks)}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Overview.self, from: Data(json.utf8))
    }

    private func node(_ id: String = "s1", project: String = "Alondra", since: Int = 1) -> String {
        #"{"session_id":"\#(id)","short_id":"\#(id)","name":"Node \#(id)","project":"\#(project)","is_orchestrator":false,"has_plugin":true,"state":"working","state_since":\#(since),"state_source":"hook","tool":{"name":"Bash","summary":"bazel test","since":2}}"#
    }

    private func task(_ id: String, title: String, node: String = "s1") -> String {
        #"{"id":"\#(id)","request":"r1","title":"\#(title)","assignee":{"session_id":"\#(node)"},"state":"running","state_since":1,"created_at":1}"#
    }

    private func operations(close: @escaping @Sendable (String) async -> NodeMutationOutcome = { _ in .obsolete },
                            send: @escaping @Sendable (String, NodeMessage) async -> NodeMutationOutcome = { _, _ in .obsolete },
                            threads: @escaping @Sendable (String) async throws -> [NodeActivity]? = { _ in nil }) -> NodeOperations {
        NodeOperations(send: send, close: close, threads: threads)
    }

    @Test func multipleTasksRemainAttachedToOneNodeAndSearchFindsEither() throws {
        let tasks = "[\(task("t1", title: "Compile")),\(task("t2", title: "Deploy"))]"
        let snapshot = ProjectFleetUseCase().execute(try overview(nodes: "[\(node())]", tasks: tasks), visibleInboxIDs: [], sequence: 1)
        #expect(snapshot.node("s1")?.tasks.map(\.id) == ["t1", "t2"])
        #expect(ProjectFleetUseCase().search(snapshot, query: "  deploy ").first?.nodes.map(\.id) == ["s1"])
        #expect(ProjectFleetUseCase().search(snapshot, query: "missing").isEmpty)
    }

    @Test func pendingCountUsesVisibleCardsAndMissingProjectUsesSharedLocalization() throws {
        let question = #"{"id":7,"session_id":"s1","title":"Done already","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":3}"#
        let overview = try overview(nodes: "[\(node(project: "others"))]", questions: "[\(question)]")
        let projected = ProjectFleetUseCase().execute(overview, visibleInboxIDs: ["q7"], sequence: 1)
        #expect(projected.node("s1")?.pendingCount == 1)
        #expect(projected.sections.first?.name == Node.otherProjectName)
        let hidden = ProjectFleetUseCase().execute(overview, visibleInboxIDs: [], sequence: 2)
        #expect(hidden.node("s1")?.pendingCount == 0)
    }

    @Test func lowerTickAfterReconnectReplacesStaleFleetAndDetail() async throws {
        let old = ProjectFleetUseCase().execute(try overview(tick: 100, nodes: "[\(node(since: 100))]"), visibleInboxIDs: [], sequence: 1)
        let fresh = ProjectFleetUseCase().execute(try overview(tick: 1, nodes: "[\(node(since: 1))]"), visibleInboxIDs: [], sequence: 2)
        let fleet = TestStore(initialState: FleetFeature.State()) { FleetFeature() } withDependencies: {
            $0.nodeOperations = operations()
        }
        await fleet.send(.snapshotChanged(old)) { $0.snapshot = old }
        await fleet.send(.snapshotUnavailable) { $0.isStale = true }
        await fleet.send(.snapshotChanged(fresh)) { $0.snapshot = fresh; $0.isStale = false }
        #expect(fleet.state.snapshot?.node("s1")?.node.stateSince == 1)
        let directRestart = ProjectFleetUseCase().execute(
            try overview(tick: 0, nodes: "[\(node(since: 0))]"), visibleInboxIDs: [], sequence: 3
        )
        await fleet.send(.snapshotChanged(directRestart)) { $0.snapshot = directRestart }
        await fleet.send(.snapshotChanged(fresh))
        #expect(fleet.state.snapshot?.tick == 0)
        await fleet.finish()

        let detail = TestStore(initialState: NodeDetailFeature.State(sessionID: "s1")) { NodeDetailFeature() } withDependencies: {
            $0.nodeOperations = operations()
        }
        await detail.send(.snapshotChanged(old)) {
            $0.acceptedTick = 100; $0.acceptedSequence = 1; $0.entry = old.node("s1")
        }
        await detail.send(.snapshotUnavailable) { $0.isStale = true }
        await detail.send(.snapshotChanged(fresh)) {
            $0.acceptedTick = 1; $0.acceptedSequence = 2; $0.entry = fresh.node("s1"); $0.isStale = false
        }
        await detail.send(.snapshotChanged(directRestart)) {
            $0.acceptedTick = 0; $0.acceptedSequence = 3; $0.entry = directRestart.node("s1")
        }
        await detail.send(.snapshotChanged(fresh))
        #expect(detail.state.node?.stateSince == 0)
        await detail.finish()
    }

    @Test func refusedCloseRetainsNodeAndStaleSnapshotBlocksRetry() async throws {
        let snapshot = ProjectFleetUseCase().execute(try overview(nodes: "[\(node())]"), visibleInboxIDs: [], sequence: 1)
        let store = TestStore(initialState: FleetFeature.State()) { FleetFeature() } withDependencies: {
            $0.nodeOperations = operations(close: { _ in .rejected("uncommitted changes") })
        }
        await store.send(.snapshotChanged(snapshot)) { $0.snapshot = snapshot }
        await store.send(.selectNode("s1")) { $0.selectedNodeID = "s1" }
        await store.send(.closeRequested("s1")) { $0.closeConfirmationID = "s1" }
        await store.send(.closeConfirmed) {
            $0.closeConfirmationID = nil
            $0.closingIDs.insert("s1")
        }
        await store.receive(\.closeFinished) {
            $0.closingIDs.remove("s1")
            $0.closeOutcomeByID["s1"] = .rejected("uncommitted changes")
            $0.closeRefusal = "uncommitted changes"
        }
        await store.send(.snapshotUnavailable) { $0.isStale = true }
        await store.send(.closeRequested("s1"))
        #expect(store.state.selectedNodeID == "s1")
        #expect(store.state.closeOutcomeByID["s1"] == .rejected("uncommitted changes"))
        await store.send(.dismissCloseRefusal) { $0.closeRefusal = nil }
        #expect(store.state.snapshot?.node("s1") != nil)
        await store.finish()
    }

    @Test func staleDetailRejectsSendAndConfirmedSendKeepsNewDraft() async throws {
        let snapshot = ProjectFleetUseCase().execute(try overview(nodes: "[\(node())]"), visibleInboxIDs: [], sequence: 1)
        var initial = NodeDetailFeature.State(sessionID: "s1")
        initial.entry = snapshot.node("s1")
        initial.acceptedTick = snapshot.tick
        initial.acceptedSequence = snapshot.sequence
        let gate = SendGate()
        let store = TestStore(initialState: initial) { NodeDetailFeature() } withDependencies: {
            $0.nodeOperations = operations(send: { _, message in
                #expect(message.text == "hello")
                return await gate.wait()
            })
        }
        await store.send(.messageChanged(" hello ")) { $0.message = " hello " }
        await store.send(.snapshotUnavailable) { $0.isStale = true }
        await store.send(.sendTapped)
        let reconnected = FleetSnapshot(sequence: 2, tick: snapshot.tick, orchestrator: snapshot.orchestrator, sections: snapshot.sections)
        await store.send(.snapshotChanged(reconnected)) { $0.acceptedSequence = 2; $0.isStale = false }
        await store.send(.sendTapped) { $0.isSending = true }
        await store.send(.messageChanged("next")) { $0.message = "next" }
        await gate.release(.confirmed)
        await store.receive(\.sendFinished) {
            $0.isSending = false
            $0.sendOutcome = .confirmed
            $0.sentCount = 1
        }
        #expect(store.state.message == "next")
        await store.finish()
    }

    @Test func threadsLoadOnlyWhenDetailIsVisibleAndOldServerFallsBackToOverview() async throws {
        let snapshot = ProjectFleetUseCase().execute(try overview(nodes: "[\(node())]"), visibleInboxIDs: [], sequence: 1)
        var initial = NodeDetailFeature.State(sessionID: "s1")
        initial.entry = snapshot.node("s1")
        initial.acceptedTick = snapshot.tick
        initial.acceptedSequence = snapshot.sequence
        let clock = TestClock()
        let store = TestStore(initialState: initial) { NodeDetailFeature() } withDependencies: {
            $0.nodeOperations = operations(threads: { _ in nil })
            $0.continuousClock = clock
        }
        await store.send(.appeared) { $0.isVisible = true }
        await store.receive(\.threadsTick) { $0.isLoadingThreads = true }
        await store.receive(\.threadsLoaded) {
            $0.isLoadingThreads = false
            $0.threads = snapshot.node("s1")!.node.toolThread
        }
        #expect(store.state.threads.map(\.doing) == ["Bash bazel test"])
        await store.send(.disappeared) { $0.isVisible = false }
        await store.finish()
    }
    @Test func openThreadsFollowSnapshotTicksAndPeriodicToolChanges() async throws {
        func snapshot(_ tick: UInt64, _ sequence: UInt64, _ summary: String) throws -> FleetSnapshot {
            ProjectFleetUseCase().execute(try overview(tick: tick, nodes: "[\(node().replacingOccurrences(of: "bazel test", with: summary))]"), visibleInboxIDs: [], sequence: sequence)
        }
        let first = try snapshot(1, 1, "make test")
        var initial = NodeDetailFeature.State(sessionID: "s1")
        initial.entry = first.node("s1"); initial.acceptedTick = first.tick; initial.acceptedSequence = first.sequence
        let clock = TestClock()
        let store = TestStore(initialState: initial) { NodeDetailFeature() } withDependencies: {
            $0.nodeOperations = operations(threads: { _ in nil })
            $0.continuousClock = clock
        }
        await store.send(.appeared) { $0.isVisible = true }
        await store.receive(\.threadsTick) { $0.isLoadingThreads = true }
        await store.receive(\.threadsLoaded) { $0.isLoadingThreads = false; $0.threads = first.node("s1")!.node.toolThread }
        let opened = store.state.acceptedTick
        let toolChange = try snapshot(1, 2, "make lint")
        await store.send(.snapshotChanged(toolChange)) { $0.acceptedSequence = 2; $0.entry = toolChange.node("s1") }
        #expect(store.state.acceptedTick == opened, "a tool call alone does not move the content tick")
        let tickChange = try snapshot(2, 3, "make lint")
        await store.send(.snapshotChanged(tickChange)) { $0.acceptedSequence = 3; $0.acceptedTick = 2; $0.entry = tickChange.node("s1") }
        #expect(store.state.acceptedTick != opened)
        await clock.advance(by: .seconds(10))
        await store.receive(\.threadsTick) { $0.isLoadingThreads = true }
        await store.receive(\.threadsLoaded) { $0.isLoadingThreads = false; $0.threads = tickChange.node("s1")!.node.toolThread }
        #expect(store.state.threads.first?.doing == "Bash make lint")
        let later = try snapshot(2, 4, "cargo test")
        await store.send(.snapshotChanged(later)) { $0.acceptedSequence = 4; $0.entry = later.node("s1") }
        await clock.advance(by: .seconds(10))
        await store.receive(\.threadsTick) { $0.isLoadingThreads = true }
        await store.receive(\.threadsLoaded) { $0.isLoadingThreads = false; $0.threads = later.node("s1")!.node.toolThread }
        #expect(store.state.threads.first?.doing == "Bash cargo test", "tool changes reach the visible detail without a content tick")
        await store.send(.disappeared) { $0.isVisible = false }
        await store.finish()
    }

    @Test func aFailedThreadReadKeepsTheLastLedgerAndOnlyFallsBackBeforeItsFirstSuccess() async throws {
        let snapshot = ProjectFleetUseCase().execute(try overview(nodes: "[\(node())]"), visibleInboxIDs: [], sequence: 1)
        let tester = NodeActivity(agentId: "a1", agentType: "tester", task: nil, tool: "mav", summary: "tap Save", since: 6)
        let replies = ThreadReplies(tester: tester)
        var initial = NodeDetailFeature.State(sessionID: "s1")
        initial.entry = snapshot.node("s1")
        let clock = TestClock()
        let store = TestStore(initialState: initial) { NodeDetailFeature() } withDependencies: {
            $0.nodeOperations = operations(threads: { _ in try await replies.read() })
            $0.continuousClock = clock
        }
        await store.send(.appeared) { $0.isVisible = true }
        await store.receive(\.threadsTick) { $0.isLoadingThreads = true }
        await store.receive(\.threadsFailed) { $0.isLoadingThreads = false; $0.threadsError = "Offline"; $0.threads = snapshot.node("s1")!.node.toolThread }
        #expect(store.state.threads.first?.doing == "Bash bazel test", "the overview is the fallback before the route answers")
        await clock.advance(by: .seconds(10))
        await store.receive(\.threadsTick) { $0.isLoadingThreads = true }
        await store.receive(\.threadsLoaded) { $0.isLoadingThreads = false; $0.threadsError = nil; $0.threads = [tester] }
        #expect(store.state.threads.map(\.thread) == ["tester"])
        await clock.advance(by: .seconds(10))
        await store.receive(\.threadsTick) { $0.isLoadingThreads = true }
        await store.receive(\.threadsFailed) { $0.isLoadingThreads = false; $0.threadsError = "Offline" }
        #expect(store.state.threads.map(\.thread) == ["tester"], "a failed ask keeps what the route last said")
        await store.send(.disappeared) { $0.isVisible = false }
        await store.finish()
    }

}

private actor SendGate {
    private var continuation: CheckedContinuation<NodeMutationOutcome, Never>?
    private var queued: NodeMutationOutcome?

    func wait() async -> NodeMutationOutcome {
        if let queued { self.queued = nil; return queued }
        return await withCheckedContinuation { continuation = $0 }
    }

    func release(_ result: NodeMutationOutcome) {
        if let continuation { self.continuation = nil; continuation.resume(returning: result) }
        else { queued = result }
    }
}

private actor ThreadReplies {
    let tester: NodeActivity
    var calls = 0
    init(tester: NodeActivity) { self.tester = tester }
    func read() throws -> [NodeActivity]? {
        calls += 1
        guard calls == 2 else { throw Offline() }
        return [tester]
    }
    private struct Offline: LocalizedError { var errorDescription: String? { "Offline" } }
}
