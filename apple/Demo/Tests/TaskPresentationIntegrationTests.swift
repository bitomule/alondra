import AlondraShared
import AlondraWorkspaceFeature
import AlondraTasksFeature
import ComposableArchitecture
import Foundation
import Testing

@Suite struct TaskPresentationIntegrationTests {
    private static let machine = #"{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}"#
    private static let node = #"{"session_id":"s1","short_id":"s1","is_orchestrator":false,"has_plugin":true,"state":"working","state_since":5,"state_source":"hook","tool":{"name":"Bash","summary":"make test","since":4}}"#

    private static func decode<T: Decodable>(_ json: String) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: Data(json.utf8))
    }

    private static func task(_ id: String, request: String = "r-1", state: String = "running", dependsOn: [String] = [], created: Int64, session: String = "s1", archivedAt: Int64? = nil) throws -> WorkTask {
        let deps = dependsOn.map { #""\#($0)""# }.joined(separator: ",")
        let archived = archivedAt.map { #","archived_at":\#($0)"# } ?? ""
        return try decode(#"{"id":"\#(id)","request":"\#(request)","title":"Task \#(id)","depends_on":[\#(deps)],"options":{"auto_merge":false},"assignee":{"session_id":"\#(session)"},"prs":[],"documents":[],"blocker":null,"notices":[],"state":"\#(state)","state_since":\#(created),"created_at":\#(created)\#(archived)}"#)
    }

    private static func request(_ id: String, tasks: [String], state: String = "in_progress", archivedAt: Int64? = nil) throws -> WorkRequest {
        let ids = tasks.map { #""\#($0)""# }.joined(separator: ",")
        let archived = archivedAt.map { #","archived_at":\#($0)"# } ?? ""
        return try decode(#"{"id":"\#(id)","text":"Do the thing\nin detail","created_at":1,"state":"\#(state)","tasks":[\#(ids)]\#(archived)}"#)
    }

    @MainActor @Test func requestsTasksCardTasksAndActivityDecode() throws {
        let node = Self.node.replacingOccurrences(of: #""state":"working""#, with: #""state":"working","activity":[{"agent_id":null,"agent_type":null,"task":"t-1","tool":"Bash","summary":"bazel build //app","since":9},{"agent_id":"a1","agent_type":"tester","task":"t-1","tool":"mav","summary":"tap Save","since":10}]"#)
        let question = #"{"id":7,"session_id":"s1","title":"Which one?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":3,"task":"t-1"}"#
        let task = #"{"id":"t-1","request":"r-1","title":"Build it","depends_on":[],"options":{"auto_merge":true,"effort":"hard","project":"alondra"},"assignee":{"session_id":"s1","agent_id":"a1"},"prs":[2],"documents":[],"blocker":{"kind":"question","question":7,"since":3},"notices":[],"state":"needs_you","state_since":3,"created_at":2,"auto_merge_pending":true}"#
        let json = #"{"tick":1,"nodes":[\#(node)],"open_questions":[\#(question)],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"requests":[{"id":"r-1","text":"Build it","created_at":1,"state":"in_progress","tasks":["t-1"]}],"tasks":[\#(task)]}"#
        let overview: Overview = try Self.decode(json)
        #expect(overview.openQuestions[0].task == "t-1")
        #expect(overview.requests?.first?.state == .inProgress)
        let decoded = try #require(overview.tasks?.first)
        #expect(decoded.state == .needsYou)
        #expect(decoded.options?.mergesAlone == true)
        #expect(decoded.mergesAlone)
        #expect(decoded.assignee?.agentId == "a1")
        #expect(overview.nodes[0].activity?.map(\.thread) == ["main", "tester"])
        let store = ServiceTaskPresentation(overview: overview)
        #expect(store.task(for: .question(overview.openQuestions[0]))?.id == "t-1")
        #expect(store.now(for: decoded) == "question: Which one?")
        #expect(store.project(of: decoded) == "alondra")
    }

    @MainActor @Test func aRunningTaskShowsItsStateAndTheNodeRouteCarriesItsThreads() throws {
        let task = #"{"id":"t-1","request":"r-1","title":"Build it","assignee":{"session_id":"s1"},"state":"running","state_since":1,"created_at":1}"#
        let overview: Overview = try Self.decode(#"{"tick":1,"nodes":[\#(Self.node)],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"requests":[],"tasks":[\#(task)]}"#)
        #expect(overview.nodes[0].activity == nil)
        let store = ServiceTaskPresentation(overview: overview)
        let running = try #require(store.board.task("t-1"))
        #expect(store.now(for: running) == nil, "the row shows “running”, not the tool in flight")
        let detail: NodeThreads = try Self.decode(#"{"node":{"session_id":"s1","activity":[{"agent_id":null,"task":"t-1","tool":"Bash","summary":"make test","since":4},{"agent_id":"a1","agent_type":"tester","tool":"mav","since":6}]},"questions":[],"timeline":[]}"#)
        #expect(detail.activity?.map(\.thread) == ["main", "tester"])
        #expect(detail.activity?.first?.doing == "Bash make test")
        let older: NodeThreads = try Self.decode(#"{"node":{"session_id":"s1"}}"#)
        #expect(older.activity == nil)
    }

    @MainActor @Test func aWaitingTaskReadsWaitingWithAnArrowWhenItWaitsInAnotherRequest() throws {
        func task(_ id: String, _ request: String, _ state: String, _ deps: String) -> String {
            #"{"id":"\#(id)","request":"\#(request)","title":"Task \#(id)","state":"\#(state)","depends_on":[\#(deps)],"state_since":1,"created_at":1,"recorded":true}"#
        }
        let tasks = [task("t-1", "r-1", "running", ""), task("t-2", "r-1", "waiting_dependency", #""t-1""#), task("t-3", "r-2", "waiting_dependency", #""t-1""#)]
        let requests = #"[{"id":"r-1","created_at":1,"state":"in_progress","tasks":["t-1","t-2"]},{"id":"r-2","created_at":1,"state":"in_progress","tasks":["t-3"]}]"#
        let store = ServiceTaskPresentation(overview: try Self.decode(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"requests":\#(requests),"tasks":[\#(tasks.joined(separator: ","))]}"#))
        let waiting = try #require(store.board.task("t-2"))
        let elsewhere = try #require(store.board.task("t-3"))
        #expect(store.board.stateLabel(of: waiting) == "waiting")
        #expect(store.board.stateLabel(of: elsewhere) == "waiting ↗")
        #expect(store.now(for: waiting) == nil, "no “after …” line: the tree already shows what it waits for")
    }

    @MainActor @Test func aStartedTaskWaitingForAMergeSaysWhichPR() throws {
        let fix = #"{"id":"t-44136","request":"r-1","title":"PhotoKit off the pool","state":"in_review","prs":[1],"state_since":1,"created_at":1,"recorded":true}"#
        let beta = #"{"id":"t-44137","request":"r-1","title":"Beta 192","state":"waiting_dependency","depends_on":["t-44136"],"awaiting":1,"state_since":2,"created_at":1,"recorded":true}"#
        let pr = #"{"id":1,"url":"https://github.com/acmecorp-private/Redraft/pull/264","repo":"acmecorp-private/Redraft","number":264,"session_id":"s1","title":"Run Home's PhotoKit calls off the pool","opened_at":1,"state":"open","additions":0,"deletions":0,"changed_files":0,"checks":[],"merge_methods":["merge"],"task":"t-44136"}"#
        let requests = #"[{"id":"r-1","created_at":1,"state":"in_progress","tasks":["t-44136","t-44137"]}]"#
        let store = ServiceTaskPresentation(overview: try Self.decode(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"prs":[\#(pr)],"requests":\#(requests),"tasks":[\#(fix),\#(beta)]}"#))
        let waiting = try #require(store.board.task("t-44137"))
        #expect(waiting.awaiting == 1)
        #expect(store.now(for: waiting) == "waits for PR #264 to be merged")
        #expect(store.waitingReason(for: waiting) == "It started, and what is left of it waits for PR #264 of “PhotoKit off the pool” to be merged: it goes on once that PR is merged.")
        let summary = try Self.decode(#"{"from":"running","to":"waiting_dependency","event":{"kind":"awaits_merge","pr":1},"at":2}"#) as WorkTask.Transition
        #expect(summary.summary == "the node has nothing left but the merge of a PR it builds on")
    }

    @MainActor @Test func aRequestGoesByItsTitleThenItsOnlyTasksThenWhatTheUserWrote() throws {
        func task(_ id: String, _ request: String) -> String {
            #"{"id":"\#(id)","request":"\#(request)","title":"Task \#(id)","state":"running","state_since":1,"created_at":1}"#
        }
        func request(_ id: String, _ state: String, _ tasks: String, _ title: String = "") -> String {
            #"{"id":"\#(id)","text":"Do the thing\nin detail","created_at":1,"state":"\#(state)","tasks":[\#(tasks)]\#(title)}"#
        }
        let requests = [
            request("r-given", "in_progress", #""a""#, #","title":"Ship the thing""#),
            request("r-one", "in_progress", #""one""#),
            request("r-two", "in_progress", #""b","c""#),
            request("r-new", "defining", ""),
        ]
        let tasks = [task("a", "r-given"), task("one", "r-one"), task("b", "r-two"), task("c", "r-two")]
        let json = #"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"requests":[\#(requests.joined(separator: ","))],"tasks":[\#(tasks.joined(separator: ","))]}"#
        let store = ServiceTaskPresentation(overview: try Self.decode(json))
        let title = { (id: String) in store.board.request(id).map(store.title(of:)) }
        #expect(title("r-given") == "Ship the thing", "the orchestrator's title wins")
        #expect(title("r-one") == "Task one", "its only task's title, not the prompt")
        #expect(title("r-two") == "Do the thing", "several tasks and no title from an older alondrad")
        #expect(title("r-new") == "Do the thing", "still defined: what the user wrote")
        #expect(try Self.request("r", tasks: []).title == nil)
    }

    @MainActor @Test func aTasksDocumentsComeFromItsOwnRouteBeyondTheOverviewsNewestReports() throws {
        let task: WorkTask = try Self.decode(#"{"id":"t","request":"r","title":"T","documents":[5],"state":"in_review","state_since":5,"created_at":1}"#)
        let details: TaskInFull = try Self.decode(#"{"task":{"id":"t","request":"r","title":"T","state":"in_review","state_since":5,"created_at":1},"reports":[{"id":5,"session_id":"s","title":"Plan","links":[],"attachments":[],"kind":"document","created_at":4}]}"#)
        let store = ServiceTaskPresentation(overview: try Self.decode(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"reports":[]}"#))
        #expect(store.documents(of: task, in: nil).isEmpty, "report 5 is not among the overview's newest")
        #expect(store.documents(of: task, in: details).map(\.title) == ["Plan"])
    }
    @MainActor @Test func aFinishedRequestCannotDispatchWithdrawal() async throws {
        let answered = try Self.request("r-3", tasks: [], state: "done")
        var initial = ServiceTasksFeature.State()
        initial.taskBoard = TaskBoard(requests: [answered], tasks: [])
        let store = Store(initialState: initial) { ServiceTasksFeature() } withDependencies: {
            $0.taskOperations = TaskOperations(
                withdraw: { _ in Issue.record("A finished request cannot be withdrawn"); return .confirmed },
                cancel: { _ in .obsolete }, reopen: { _ in .obsolete }, archiveCompleted: { .obsolete }, details: { _ in nil })
        }
        await store.send(.withdrawRequested(answered.id)).finish()
        await store.send(.withdrawConfirmed).finish()
        #expect(store.state.withdrawingRequestID == nil)
        #expect(store.state.withdrawOutcome == nil, "nothing was sent for it")
    }

}
