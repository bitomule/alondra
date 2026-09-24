import AlondraShared
import Foundation
import Testing

@Suite struct TaskBoardTests {
    private static func decode<T: Decodable>(_ json: String) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: Data(json.utf8))
    }

    private static let machine = #"{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}"#
    private static let node = #"{"session_id":"s1","short_id":"s1","is_orchestrator":false,"has_plugin":true,"state":"working","state_since":5,"state_source":"hook","tool":{"name":"Bash","summary":"make test","since":4}}"#

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

    @MainActor @Test func anOverviewFromBeforeTasksStillDecodes() throws {
        let overview: Overview = try Self.decode(#"{"tick":1,"nodes":[\#(Self.node)],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine)}"#)
        #expect(overview.requests == nil)
        #expect(overview.tasks == nil)
        #expect(overview.nodes[0].activity == nil)
        let store = FleetStore()
        store.apply(overview)
        #expect(store.board.trees.isEmpty)
        #expect(store.nodes[0].toolThread.map(\.doing) == ["Bash make test"], "the tool in flight stands in for the ledger")
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
        let store = FleetStore()
        store.apply(overview)
        #expect(store.task(for: .question(overview.openQuestions[0]))?.id == "t-1")
        #expect(store.now(for: decoded) == "question: Which one?")
        #expect(store.project(of: decoded) == "alondra")
    }

    /// The overview no longer carries what each thread does; a running task shows its state, and
    /// the node's own route says what its threads do when its detail opens.
    @MainActor @Test func aRunningTaskShowsItsStateAndTheNodeRouteCarriesItsThreads() throws {
        let task = #"{"id":"t-1","request":"r-1","title":"Build it","assignee":{"session_id":"s1"},"state":"running","state_since":1,"created_at":1}"#
        let overview: Overview = try Self.decode(#"{"tick":1,"nodes":[\#(Self.node)],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"requests":[],"tasks":[\#(task)]}"#)
        #expect(overview.nodes[0].activity == nil)
        let store = FleetStore()
        store.apply(overview)
        let running = try #require(store.board.task("t-1"))
        #expect(store.now(for: running) == nil, "the row shows “running”, not the tool in flight")
        let detail: NodeThreads = try Self.decode(#"{"node":{"session_id":"s1","activity":[{"agent_id":null,"task":"t-1","tool":"Bash","summary":"make test","since":4},{"agent_id":"a1","agent_type":"tester","tool":"mav","since":6}]},"questions":[],"timeline":[]}"#)
        #expect(detail.activity?.map(\.thread) == ["main", "tester"])
        #expect(detail.activity?.first?.doing == "Bash make test")
        let older: NodeThreads = try Self.decode(#"{"node":{"session_id":"s1"}}"#)
        #expect(older.activity == nil)
    }

    /// Round 2: a node's detail asked its threads once, when it opened, and never again while it
    /// stayed open.
    @MainActor @Test func anOpenNodesThreadsAreAskedAgainWhenTheTickMovesAndWhileItShows() async throws {
        let overview = { (tick: Int, summary: String) throws -> Overview in
            let node = Self.node.replacingOccurrences(of: "make test", with: summary)
            return try Self.decode(#"{"tick":\#(tick),"nodes":[\#(node)],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine)}"#)
        }
        let store = FleetStore(client: AlondraClient(base: try #require(URL(string: "http://127.0.0.1:9"))))
        store.apply(try overview(1, "make test"))
        let opened = store.threadsKey(of: "s1")
        store.apply(try overview(1, "make lint"))
        #expect(store.threadsKey(of: "s1") == opened, "a tool call alone does not restart it")
        store.apply(try overview(2, "make lint"))
        #expect(store.threadsKey(of: "s1") != opened, "a state changed: asked again")

        let shown = Shown()
        let following = Task { await store.followThreads(of: "s1", every: .milliseconds(20)) { shown.doing.append($0.first?.doing) } }
        defer { following.cancel() }
        for _ in 0..<100 where shown.doing.isEmpty { try await Task.sleep(for: .milliseconds(10)) }
        #expect(shown.doing.first == "Bash make lint")
        store.apply(try overview(2, "cargo test"))
        for _ in 0..<100 where shown.doing.last != "Bash cargo test" { try await Task.sleep(for: .milliseconds(10)) }
        #expect(shown.doing.last == "Bash cargo test", "what it does now reaches the open detail without a tick")
    }

    /// Round 2: a request the orchestrator never defined nor closed had no way out for the user.
    @MainActor @Test func onlyARequestStillBeingDefinedCanBeWithdrawn() async throws {
        let defining = try Self.request("r-1", tasks: [], state: "defining")
        #expect(defining.canWithdraw)
        #expect(try Self.request("r-2", tasks: ["t-1"]).canWithdraw == false, "one with tasks ends through them")
        let answered = try Self.request("r-3", tasks: [], state: "done")
        #expect(answered.canWithdraw == false)
        let store = FleetStore(client: AlondraClient(base: try #require(URL(string: "http://127.0.0.1:9"))))
        #expect(await store.withdraw(answered) == false)
        #expect(store.notice == nil, "nothing was sent for it")
    }

    @Test func aCardWithoutItsTaskIsFoundThroughWhatTheTaskCarries() throws {
        let question: Question = try Self.decode(#"{"id":7,"session_id":"s1","title":"Q","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":3}"#)
        let blocked: WorkTask = try Self.decode(#"{"id":"t-9","request":"r-1","title":"T","blocker":{"kind":"question","question":7,"since":3},"state":"needs_you","state_since":3,"created_at":2}"#)
        let board = TaskBoard(requests: [], tasks: [blocked])
        #expect(question.task == nil)
        #expect(board.task(for: .question(question))?.id == "t-9")
    }

    @Test func anUnknownTaskStateReadsAsRunning() throws {
        let task: WorkTask = try Self.decode(#"{"id":"t","request":"r","title":"T","state":"paused_by_moon","state_since":1,"created_at":1}"#)
        #expect(task.state == .running)
        #expect(task.dependsOn.isEmpty)
    }

    /// A then B then C: each goes under the one before it.
    @Test func sequentialTasksNestUnderTheOneTheyFollow() throws {
        let tasks = [try Self.task("a", created: 1), try Self.task("b", dependsOn: ["a"], created: 2), try Self.task("c", dependsOn: ["b"], created: 3)]
        let board = TaskBoard(requests: [try Self.request("r-1", tasks: ["a", "b", "c"])], tasks: tasks)
        let rows = try #require(board.trees.first).rows
        #expect(rows.map(\.id) == ["a", "b", "c"])
        #expect(rows.map(\.depth) == [0, 1, 2])
        #expect(rows.map(\.parent) == [nil, "a", "b"])
        #expect(rows.map(\.hasChildren) == [true, true, false])
        #expect(rows[2].linesThrough == [false], "b is a's only child, so a's line stops at b")
    }

    /// A then B and C in parallel after A; D in parallel with A.
    @Test func parallelTasksShareALevel() throws {
        let tasks = [
            try Self.task("a", created: 1),
            try Self.task("b", dependsOn: ["a"], created: 2),
            try Self.task("c", dependsOn: ["a"], created: 3),
            try Self.task("d", created: 4),
        ]
        let board = TaskBoard(requests: [try Self.request("r-1", tasks: ["a", "b", "c", "d"])], tasks: tasks)
        let rows = try #require(board.trees.first).rows
        #expect(rows.map(\.id) == ["a", "b", "c", "d"])
        #expect(rows.map(\.depth) == [0, 1, 1, 0])
        #expect(rows.map(\.isLastSibling) == [false, false, true, true])
        #expect(board.mergeOrder(around: tasks[0]).map(\.id) == ["a", "b", "c"])
        #expect(board.mergeOrder(around: tasks[2]).map(\.id) == ["a", "c"])
    }

    @Test func aTaskWithTwoDependenciesGoesUnderTheNewestAndACycleStillShowsEveryTask() throws {
        let joined = [try Self.task("a", created: 1), try Self.task("b", created: 2), try Self.task("c", dependsOn: ["a", "b"], created: 3)]
        let rows = try #require(TaskBoard(requests: [try Self.request("r", tasks: ["a", "b", "c"])], tasks: joined).trees.first).rows
        #expect(rows.map(\.id) == ["a", "b", "c"])
        #expect(rows.map(\.parent) == [nil, nil, "b"])
        let cycle = [try Self.task("x", dependsOn: ["y"], created: 1), try Self.task("y", dependsOn: ["x"], created: 2)]
        let looped = try #require(TaskBoard(requests: [try Self.request("r", tasks: ["x", "y"])], tasks: cycle).trees.first).rows
        #expect(Set(looped.map(\.id)) == ["x", "y"])
    }

    @Test func theSidebarGroupsWhatNeedsTheUserFirstAndCountsOpenWork() throws {
        let tasks = [
            try Self.task("run", state: "running", created: 1),
            try Self.task("ask", state: "needs_you", created: 2),
            try Self.task("pr", state: "in_review", created: 3),
            try Self.task("old", state: "done", created: 4),
        ]
        let board = TaskBoard(requests: [try Self.request("r-1", tasks: tasks.map(\.id)), try Self.request("r-2", tasks: [], state: "defining")], tasks: tasks)
        #expect(board.groups.map(\.kind) == [.task(.needsYou), .task(.inReview), .task(.running), .defining, .task(.done)])
        #expect(board.openCount == 4)
        #expect(board.openTrees.map(\.id) == ["r-2", "r-1"])
    }

    @Test func archivedAtIsReadWhenSentAndOptionalOtherwise() throws {
        let plain: WorkTask = try Self.decode(#"{"id":"t","request":"r","title":"T","state":"done","state_since":1,"created_at":1}"#)
        let archived: WorkTask = try Self.decode(#"{"id":"t","request":"r","title":"T","state":"done","state_since":1,"created_at":1,"archived_at":9}"#)
        #expect(plain.archivedAt == nil && !plain.isArchived)
        #expect(archived.archivedAt == 9 && archived.isArchived)
        let older = try Self.request("r", tasks: ["t"])
        let unarchived: WorkRequest = try Self.decode(#"{"id":"r","created_at":1,"state":"in_progress","tasks":["t"],"archived_at":null}"#)
        let archivedRequest: WorkRequest = try Self.decode(#"{"id":"r","created_at":1,"state":"done","tasks":["t"],"archived_at":9}"#)
        #expect(older.archivedAt == nil && unarchived.archivedAt == nil && archivedRequest.archivedAt == 9)
    }

    @Test func archivedWorkLeavesTheListForAFoldedSectionAndARunningTaskIsNeverArchivable() throws {
        let tasks = [
            try Self.task("shipped", request: "r-1", state: "done", created: 1, archivedAt: 9),
            try Self.task("dropped", request: "r-1", state: "cancelled", created: 2, archivedAt: 9),
            try Self.task("old", request: "r-2", state: "done", created: 3, archivedAt: 9),
            try Self.task("run", request: "r-2", state: "running", created: 4),
        ]
        let requests = [
            try Self.request("r-1", tasks: ["shipped", "dropped"], state: "done", archivedAt: 9),
            try Self.request("r-2", tasks: ["old", "run"]),
            try Self.request("r-3", tasks: [], state: "done"),
            try Self.request("r-4", tasks: [], state: "done", archivedAt: 9),
        ]
        let board = TaskBoard(requests: requests, tasks: tasks)
        #expect(board.activeTrees.map(\.id) == ["r-3", "r-2"])
        #expect(board.activeTrees.first { $0.id == "r-2" }?.rows.map(\.id) == ["run"])
        #expect(board.archivedTrees.map(\.id) == ["r-4", "r-2", "r-1"])
        #expect(board.archivedTrees.first { $0.id == "r-2" }?.rows.map(\.id) == ["old"])
        #expect(board.archivedCount == 4, "three archived tasks and a request archived without tasks")
        #expect(board.doneTrees.map(\.id) == ["r-3"], "finished without tasks: done, waiting to be archived")
        #expect(board.hasArchivable)
        #expect(board.tasks(onNode: "s1").map(\.id) == ["run"])
        #expect(board.groups.map(\.kind) == [.task(.running), .task(.done), .archived])
        #expect(board.groups.last?.count == 4)

        let running = TaskBoard(requests: [try Self.request("r", tasks: ["run"])], tasks: [try Self.task("run", request: "r", created: 1)])
        #expect(!running.hasArchivable, "a running task is never archived")
        #expect(running.archivedTrees.isEmpty && running.archivedCount == 0)
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
        let store = FleetStore()
        store.apply(try Self.decode(json))
        let title = { (id: String) in store.board.request(id).map(store.title(of:)) }
        #expect(title("r-given") == "Ship the thing", "the orchestrator's title wins")
        #expect(title("r-one") == "Task one", "its only task's title, not the prompt")
        #expect(title("r-two") == "Do the thing", "several tasks and no title from an older alondrad")
        #expect(title("r-new") == "Do the thing", "still defined: what the user wrote")
        #expect(try Self.request("r", tasks: []).title == nil)
    }

    @MainActor @Test func aTasksHistoryFollowsItsLastChangeAndAFailedFetchKeepsWhatWasShown() async throws {
        let older = try Self.task("t", created: 5)
        let newer: WorkTask = try Self.decode(#"{"id":"t","request":"r","title":"T","state":"running","state_since":5,"created_at":1,"last_change":9}"#)
        #expect(older.historyKey == 5 && newer.historyKey == 9, "a move that keeps the state still moves the key")
        let wrapped: TaskInFull = try Self.decode(#"{"task":{"id":"t","request":"r","title":"T","state":"running","state_since":5,"created_at":1,"history":[{"from":"running","to":"in_review","event":{"kind":"pr_opened","pr":2},"at":6}]},"reports":[{"id":5,"session_id":"s","title":"Plan","links":[],"attachments":[],"kind":"document","created_at":4}]}"#)
        let bare: TaskInFull = try Self.decode(#"{"id":"t","request":"r","title":"T","state":"running","state_since":5,"created_at":1}"#)
        #expect(wrapped.task.history?.count == 1 && wrapped.documents?.map(\.id) == [5])
        #expect(bare.task.id == "t" && bare.documents == nil, "an older alondrad sends the task alone")
        let nowhere = AlondraClient(base: try #require(URL(string: "http://127.0.0.1:9")))
        let store = FleetStore(client: nowhere)
        #expect(await store.details(of: newer, keeping: wrapped)?.task.history?.count == 1, "a failed fetch keeps the history shown")
    }

    @MainActor @Test func aTasksDocumentsComeFromItsOwnRouteBeyondTheOverviewsNewestReports() throws {
        let task: WorkTask = try Self.decode(#"{"id":"t","request":"r","title":"T","documents":[5],"state":"in_review","state_since":5,"created_at":1}"#)
        let details: TaskInFull = try Self.decode(#"{"task":{"id":"t","request":"r","title":"T","state":"in_review","state_since":5,"created_at":1},"reports":[{"id":5,"session_id":"s","title":"Plan","links":[],"attachments":[],"kind":"document","created_at":4}]}"#)
        let store = FleetStore()
        store.apply(try Self.decode(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine),"reports":[]}"#))
        #expect(store.documents(of: task, in: nil).isEmpty, "report 5 is not among the overview's newest")
        #expect(store.documents(of: task, in: details).map(\.title) == ["Plan"])
    }

    @Test func aTasksPrHandlingReadsLikeTheLaunchOption() throws {
        let spec: OptionSpec = try Self.decode(#"{"id":"pr","label":"PR handling","kind":"choice","builtin":true,"default":"review","values":[{"id":"review","label":"review each PR"},{"id":"auto_merge","label":"auto-merge"}]}"#)
        #expect(spec.values.map(spec.title(of:)) == [OptionSpec.prHandling(autoMerge: false), OptionSpec.prHandling(autoMerge: true)])
    }
}

/// What a node's detail was shown, in order.
@MainActor private final class Shown {
    var doing: [String?] = []
}
