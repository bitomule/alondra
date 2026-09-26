import AlondraContracts
import AlondraDomain
import Foundation
import Testing

@Suite struct TaskBoardTests {
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

    private static func work(_ id: String, request: String, title: String? = nil, state: String = "running", at: Int64, recorded: Bool = true, session: String? = nil, prs: [Int] = [], dependsOn: [String] = [], lastChange: Int64? = nil) throws -> WorkTask {
        let assignee = session.map { #","assignee":{"session_id":"\#($0)"}"# } ?? ""
        let change = lastChange.map { #","last_change":\#($0)"# } ?? ""
        let deps = dependsOn.map { #""\#($0)""# }.joined(separator: ",")
        let numbers = prs.map(String.init).joined(separator: ",")
        return try decode(#"{"id":"\#(id)","request":"\#(request)","title":"\#(title ?? "Task \(id)")","state":"\#(state)","state_since":\#(at),"created_at":\#(at),"recorded":\#(recorded),"prs":[\#(numbers)],"depends_on":[\#(deps)]\#(assignee)\#(change)}"#)
    }

    private static func asked(_ id: String, tasks: [String], state: String = "in_progress", synthetic: Bool = false, created: Int64 = 1, title: String? = nil, closedAt: Int64? = nil) throws -> WorkRequest {
        let ids = tasks.map { #""\#($0)""# }.joined(separator: ",")
        let named = title.map { #","title":"\#($0)""# } ?? ""
        let closed = closedAt.map { #","closed":{"at":\#($0)}"# } ?? ""
        return try decode(#"{"id":"\#(id)","text":"Do the thing","created_at":\#(created),"state":"\#(state)","synthetic":\#(synthetic),"tasks":[\#(ids)]\#(named)\#(closed)}"#)
    }

    @Test func anOverviewFromBeforeTasksStillDecodes() throws {
        let overview: Overview = try Self.decode(#"{"tick":1,"nodes":[\#(Self.node)],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":\#(Self.machine)}"#)
        #expect(overview.requests == nil)
        #expect(overview.tasks == nil)
        #expect(overview.nodes[0].activity == nil)
        let board = TaskBoard(requests: overview.requests ?? [], tasks: overview.tasks ?? [])
        #expect(board.trees.isEmpty)
        #expect(overview.nodes[0].toolThread.map(\.doing) == ["Bash make test"], "the tool in flight stands in for the ledger")
    }

    @Test func onlyARequestStillBeingDefinedCanBeWithdrawn() throws {
        let defining = try Self.request("r-1", tasks: [], state: "defining")
        #expect(defining.canWithdraw)
        #expect(try Self.request("r-2", tasks: ["t-1"]).canWithdraw == false, "one with tasks ends through them")
        let answered = try Self.request("r-3", tasks: [], state: "done")
        #expect(answered.canWithdraw == false)
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
        #expect(board.openTrees.map(\.id) == ["r-1", "r-2"], "r-1's tasks moved after r-2 was sent")
    }

    @Test func probeSessionsLeaveTheListsAndTheCountsButNothingOfARequestDavidMade() throws {
        let tasks = [
            try Self.work("t-new", request: "r-view", at: 100, session: "s1"),
            try Self.work("t-s1", request: "r-old", title: "alondra/estados-por-tarea", at: 10, recorded: false, session: "s1", prs: [83]),
            try Self.work("t-shift", request: "r-shift", title: "test/shift-x", state: "cancelled", at: 5, recorded: false),
            try Self.work("t-inject", request: "r-inject", title: "prompt injection test", state: "cancelled", at: 6, recorded: false),
            try Self.work("t-rama", request: "r-rama", title: "alondra/rama-main", state: "done", at: 7, recorded: false, prs: [65]),
            try Self.work("t-gone", request: "r-mine", title: "alondra/abandonada", state: "cancelled", at: 8, recorded: false),
        ]
        let requests = [
            try Self.asked("r-view", tasks: ["t-new"], created: 99),
            try Self.asked("r-old", tasks: ["t-s1"], created: 9),
            try Self.asked("r-shift", tasks: ["t-shift"], state: "done", synthetic: true),
            try Self.asked("r-inject", tasks: ["t-inject"], state: "done", synthetic: true),
            try Self.asked("r-rama", tasks: ["t-rama"], state: "done", synthetic: true),
            try Self.asked("r-mine", tasks: ["t-gone"], state: "done"),
        ]
        let board = TaskBoard(requests: requests, tasks: tasks)
        #expect(board.hidden == ["t-shift", "t-inject"])
        #expect(board.openTrees.map(\.id) == ["r-view", "r-old"])
        #expect(board.doneTrees.map(\.id) == ["r-mine", "r-rama"], "a session that delivered stays, and so does anything of a request David made")
        #expect(board.openCount == 2)
        #expect(board.tasks(onNode: "s1").map(\.id) == ["t-s1", "t-new"])
        #expect(board.groups.flatMap(\.tasks).map(\.id).sorted() == ["t-gone", "t-new", "t-rama", "t-s1"])
    }

    @Test func aRequestWithOneTaskIsOneRowUnderTheOrchestratorsTitleOrTheTasks() throws {
        let tasks = [
            try Self.work("a", request: "r-titled", at: 1),
            try Self.work("b", request: "r-two", at: 2),
            try Self.work("c", request: "r-two", at: 3),
            try Self.work("d", request: "r-plain", at: 4),
        ]
        let board = TaskBoard(
            requests: [
                try Self.asked("r-titled", tasks: ["a"], title: "Readable Tasks view"),
                try Self.asked("r-two", tasks: ["b", "c"], title: "Two parts"),
                try Self.asked("r-plain", tasks: ["d"]),
            ],
            tasks: tasks
        )
        #expect(board.rowTitle(of: tasks[0]) == "Readable Tasks view")
        #expect(board.rowTitle(of: tasks[1]) == "Task b", "the request's title heads its tree, not each row")
        #expect(board.rowTitle(of: tasks[3]) == "Task d")
    }

    @Test func openWorkGoesByItsLastChangeAndFinishedWorkTheLastToFinishFirst() throws {
        let tasks = [
            try Self.work("a", request: "r-a", at: 50),
            try Self.work("b", request: "r-b", at: 30, lastChange: 90),
            try Self.work("x", request: "r-x", state: "done", at: 40),
            try Self.work("y", request: "r-y", state: "cancelled", at: 70),
        ]
        let requests = [
            try Self.asked("r-a", tasks: ["a"], created: 1),
            try Self.asked("r-b", tasks: ["b"], created: 2),
            try Self.asked("r-defining", tasks: [], state: "defining", created: 60),
            try Self.asked("r-x", tasks: ["x"], state: "done", created: 3),
            try Self.asked("r-y", tasks: ["y"], state: "done", created: 4),
            try Self.asked("r-answered", tasks: [], state: "done", created: 5, closedAt: 55),
        ]
        let board = TaskBoard(requests: requests, tasks: tasks)
        #expect(board.openTrees.map(\.id) == ["r-b", "r-defining", "r-a"])
        #expect(board.doneTrees.map(\.id) == ["r-y", "r-answered", "r-x"])
        #expect(board.doneTrees.first?.isCancelled == true, "every task of it was cancelled: it does not read as done")
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
        #expect(board.activeTrees.map(\.id) == ["r-2", "r-3"], "what is open comes before what finished")
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

    @Test func taskHistoryKeysAndWrappedOrBareDetailsDecode() throws {
        let older = try Self.task("t", created: 5)
        let newer: WorkTask = try Self.decode(#"{"id":"t","request":"r","title":"T","state":"running","state_since":5,"created_at":1,"last_change":9}"#)
        #expect(older.historyKey == 5 && newer.historyKey == 9, "a move that keeps the state still moves the key")
        let wrapped: TaskInFull = try Self.decode(#"{"task":{"id":"t","request":"r","title":"T","state":"running","state_since":5,"created_at":1,"history":[{"from":"running","to":"in_review","event":{"kind":"pr_opened","pr":2},"at":6}]},"reports":[{"id":5,"session_id":"s","title":"Plan","links":[],"attachments":[],"kind":"document","created_at":4}]}"#)
        let bare: TaskInFull = try Self.decode(#"{"id":"t","request":"r","title":"T","state":"running","state_since":5,"created_at":1}"#)
        #expect(wrapped.task.history?.count == 1 && wrapped.documents?.map(\.id) == [5])
        #expect(bare.task.id == "t" && bare.documents == nil, "an older alondrad sends the task alone")
    }
}
