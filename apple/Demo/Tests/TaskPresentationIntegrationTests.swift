import AlondraContracts
import AlondraDomain
import AlondraShared
import AlondraWorkspaceFeature
import AlondraTasksFeature
import ComposableArchitecture
import Foundation
import Testing

@Suite struct TaskPresentationIntegrationTests {
    private static func decode<T: Decodable>(_ json: String) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: Data(json.utf8))
    }

    @MainActor @Test func tasksCardsAndRuntimeMeetInThePresentation() throws {
        var fixture = V2Fixture()
        fixture.tasks = [V2Fixture.makeTask("t1", state: .needsYou)]
        let question = fixture.question("q7", title: "Which one?")
        fixture.runtime = V2Fixture.runtime(#""nodes":[],"tasks":{"t1":{"project":"alondra","effort":"hard","state_since":3}},"requests":{"request-1":{"created_at":1}}"#)
        let store = ServiceTaskPresentation(workspace: fixture.workspace)
        let task = try #require(store.board.task("t1"))
        #expect(store.task(for: question)?.id == "t1")
        #expect(store.now(for: task) == "question: Which one?")
        #expect(store.project(of: task) == "alondra")
        let context = ServiceTaskContext(presentation: store, taskID: "t1")
        #expect(context.requestTitle == "Task t1", "a request with one task goes by that task's title")
        #expect(context.requestCreatedAt == 1)
        #expect(context.project == "alondra")
        #expect(context.now == "question: Which one?")
        #expect(context.effort == "hard")
        #expect(context.stateSince == 3)
        #expect(context.requestTaskCount == 1)
        #expect(context.pending.map(\.id) == [question.id])
        let resolved = ServiceTaskContext(presentation: ServiceTaskPresentation(workspace: fixture.workspace, hiddenIDs: [question.id]), taskID: "t1")
        #expect(resolved.pending.isEmpty)
        #expect(resolved.requestTitle == "Task t1")
        #expect(resolved.requestTaskCount == 1)
    }

    @MainActor @Test func aRunningTaskShowsItsStateAndTheNodeRouteCarriesItsThreads() throws {
        let fixture = V2Fixture()
        let store = ServiceTaskPresentation(workspace: fixture.workspace)
        let running = try #require(store.board.task("t1"))
        #expect(store.now(for: running) == nil, "the row shows “running”, not the tool in flight")
        let detail: NodeThreads = try Self.decode(#"{"node":{"session_id":"s1","activity":[{"agent_id":null,"task":"t1","tool":"Bash","summary":"make test","since":4},{"agent_id":"a1","agent_type":"tester","tool":"mav","since":6}]},"questions":[],"timeline":[]}"#)
        #expect(detail.activity?.map(\.thread) == ["main", "tester"])
        #expect(detail.activity?.first?.doing == "Bash make test")
        let older: NodeThreads = try Self.decode(#"{"node":{"session_id":"s1"}}"#)
        #expect(older.activity == nil)
    }

    @MainActor @Test func everyServerTaskStateKeepsItsOwnLabel() throws {
        var fixture = V2Fixture()
        let states: [DomainV2.TaskState] = [.notStarted, .running, .needsYou, .inReview, .waitingDependency, .closing, .done, .cancelled]
        fixture.tasks = states.enumerated().map { V2Fixture.makeTask(DomainV2.TaskId(rawValue: "t\($0.offset)"), state: $0.element) }
        let store = ServiceTaskPresentation(workspace: fixture.workspace)
        let labels = try states.indices.map { try #require(store.board.task(DomainV2.TaskId(rawValue: "t\($0)"))).state.label }
        #expect(labels == ["not started", "running", "needs you", "presented", "waiting for a dependency",
                           "accepted, pending the agent", "finished", "cancelled"])
        #expect(Set(labels).count == states.count, "no two server states share a label")
    }

    @MainActor @Test func aStartedTaskWaitingForAMergeSaysWhichPR() throws {
        var fixture = V2Fixture()
        fixture.pullRequests = [.init(id: "pr-264", taskId: "fix", host: "github.com", repository: "bitomule/Redraft", number: 264,
                                      head: "h", base: "main", state: .open, mergedHead: nil, allowedMethods: [.merge], review: nil)]
        var fix = V2Fixture.makeTask("fix", state: .inReview, prId: "pr-264")
        fix.title = "PhotoKit off the pool"
        var beta = V2Fixture.makeTask("beta", state: .waitingDependency)
        beta.dependencies = ["fix"]
        beta.stack = .init(upstreamTask: "fix", presentationId: "p", prId: "pr-264", head: "h")
        fixture.tasks = [fix, beta]
        let store = ServiceTaskPresentation(workspace: fixture.workspace)
        let waiting = try #require(store.board.task("beta"))
        #expect(store.now(for: waiting) == "waits for PR #264 to be merged")
        #expect(store.waitingReason(for: waiting) == "It started, and what is left of it waits for PR #264 of “PhotoKit off the pool” to be merged: it goes on once that PR is merged.")
    }

    @MainActor @Test func aRequestGoesByItsTitleThenItsOnlyTaskThenWhatTheUserWrote() throws {
        var fixture = V2Fixture()
        fixture.requests = ["r-given", "r-one", "r-two", "r-new"].map { .init(id: .init(rawValue: $0), body: "Do the thing\nin detail", closed: false) }
        fixture.requests[0].title = "Ship the thing"
        fixture.tasks = [V2Fixture.makeTask("a", request: "r-given"), V2Fixture.makeTask("one", request: "r-one"),
                         V2Fixture.makeTask("b", request: "r-two"), V2Fixture.makeTask("c", request: "r-two")]
        let store = ServiceTaskPresentation(workspace: fixture.workspace)
        let title = { (id: String) in store.board.request(.init(rawValue: id)).map(store.title(of:)) }
        #expect(title("r-given") == "Ship the thing", "the orchestrator's title wins")
        #expect(title("r-one") == "Task one", "its only task's title, not the prompt")
        #expect(title("r-two") == "Do the thing", "several tasks and no title: what the user wrote")
        #expect(title("r-new") == "Do the thing", "still being split: what the user wrote")
        #expect(store.definingLabel(of: try #require(store.board.request("r-new"))) == "the orchestrator is splitting it")
    }

    @MainActor @Test func aTasksDocumentsComeFromItsDocumentMap() throws {
        var fixture = V2Fixture()
        fixture.documentReview("plan")
        fixture.tasks[0].documents = ["doc-plan": "plan"]
        let store = ServiceTaskPresentation(workspace: fixture.workspace)
        let task = try #require(store.board.task("t1"))
        #expect(store.documents(of: task).map(\.document.title) == ["Document plan"])
        let context = ServiceTaskContext(presentation: store, taskID: "t1")
        #expect(context.documentItems["doc-plan"] == fixture.inbox.last?.id, "the waiting review card opens the document")
        let reviewed = ServiceTaskContext(presentation: ServiceTaskPresentation(workspace: fixture.workspace, hiddenIDs: [fixture.inbox.last!.id]), taskID: "t1")
        #expect(reviewed.documentItems["doc-plan"] == nil)
    }

    @MainActor @Test func aFinishedRequestCannotDispatchWithdrawal() async throws {
        var fixture = V2Fixture()
        fixture.requests = [.init(id: "r-3", body: "Done already", closed: true)]
        fixture.tasks = []
        let log = CommandLog()
        let store = Store(initialState: ServiceTasksFeature.State()) { ServiceTasksFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in
                Issue.record("A finished request cannot be withdrawn")
                return V2Fixture.recorded
            }
        }
        await store.send(.workspaceChanged(fixture.workspace)).finish()
        await store.send(.withdrawRequested("r-3")).finish()
        await store.send(.withdrawConfirmed).finish()
        #expect(store.state.withdrawingRequestID == nil)
        #expect(store.state.withdrawOutcome == nil, "nothing was sent for it")
        #expect(log.count == 0)
    }

    @MainActor @Test func withdrawingAgainAfterAnUnknownOutcomeReusesItsCommandID() async throws {
        let fixture = V2Fixture()
        let log = CommandLog()
        let results = LockIsolated<[DomainCommandResult]>([.uncertain, V2Fixture.recorded])
        let store = Store(initialState: ServiceTasksFeature.State()) { ServiceTasksFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.domainWorkspace = .scripted(log) { intent in
                #expect(intent == .closeRequest(V2Fixture.request))
                return results.withValue { $0.removeFirst() }
            }
        }
        await store.send(.workspaceChanged(fixture.workspace)).finish()
        await store.send(.withdrawRequested(V2Fixture.request)).finish()
        await store.send(.withdrawConfirmed).finish()
        #expect(store.state.withdrawOutcome == .unknown)
        await store.send(.withdrawRequested(V2Fixture.request)).finish()
        await store.send(.withdrawConfirmed).finish()
        #expect(store.state.withdrawOutcome == .confirmed)
        #expect(log.count == 2)
        #expect(Set(log.commandIDs).count == 1, "the retry is the same command, so the server applies it once")
        #expect(store.state.withdrawCommandIDs.isEmpty)
    }
}
