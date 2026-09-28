import AlondraContracts
import AlondraDomain
import Foundation
import Testing

@Suite struct TaskBoardTests {
    private static func task(_ id: String, request: String = "r-1", state: DomainV2.TaskState = .running,
                             dependencies: [String] = [], agent: String? = "a", archived: Bool = false) -> DomainV2.Task {
        .init(id: .init(rawValue: id), requestId: .init(rawValue: request), agentId: agent.map { .init(rawValue: $0) },
              title: "Task \(id)", state: state, revision: 1, started: true, reviewPrs: false, finalPresentation: nil,
              prId: nil, dependencies: dependencies.map { .init(rawValue: $0) }, stack: nil, documents: [:], archived: archived)
    }

    private static func request(_ id: String, closed: Bool = false) -> DomainV2.Request {
        .init(id: .init(rawValue: id), body: "Do the thing", closed: closed)
    }

    @Test func sequentialTasksNestUnderTheOneTheyFollowAndParallelOnesShareALevel() {
        let board = TaskBoard(requests: [Self.request("r-1")],
                              tasks: [Self.task("a"), Self.task("b", dependencies: ["a"]), Self.task("c")])
        let rows = board.tree(for: "r-1")?.rows ?? []
        #expect(rows.map(\.id) == ["a", "b", "c"])
        #expect(rows.map(\.depth) == [0, 1, 0])
        #expect(rows[1].parent == "a")
    }

    @Test func aDependencyCycleStillShowsEveryTask() {
        let board = TaskBoard(requests: [Self.request("r-1")],
                              tasks: [Self.task("a", dependencies: ["b"]), Self.task("b", dependencies: ["a"])])
        #expect(Set(board.tree(for: "r-1")?.rows.map(\.id) ?? []) == ["a", "b"])
    }

    @Test func groupsKeepEveryServerStateApartAndArchivedWorkIsFolded() {
        let board = TaskBoard(requests: [Self.request("r-1")], tasks: [
            Self.task("a", state: .needsYou), Self.task("b", state: .closing), Self.task("c", state: .running),
            Self.task("d", state: .done, archived: true), Self.task("e", state: .done),
        ])
        #expect(board.groups.map(\.kind) == [.task(.needsYou), .task(.closing), .task(.running), .task(.done), .archived])
        #expect(board.openCount == 3)
        #expect(board.archivedTrees.map(\.id) == ["r-1"])
        #expect(board.archivedCount == 1)
    }

    @Test func anAgentWithSeveralTasksKeepsThemAllWhenOneFinishes() {
        let board = TaskBoard(requests: [Self.request("r-1"), Self.request("r-2")], tasks: [
            Self.task("a", state: .done), Self.task("b", request: "r-2"), Self.task("c", request: "r-2", agent: "other"),
        ])
        #expect(board.tasks(assignedTo: "a").map(\.id) == ["a", "b"])
        #expect(board.openTrees.map(\.id) == ["r-2"])
        #expect(board.doneTrees.map(\.id) == ["r-1"])
    }

    @Test func aClosedRequestWithoutTasksLeavesTheList() {
        let board = TaskBoard(requests: [Self.request("open"), Self.request("closed", closed: true)], tasks: [])
        #expect(board.activeTrees.map(\.id) == ["open"])
    }
}
