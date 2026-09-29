import AlondraShared
import Foundation
import Testing

@Suite struct TaskPresentationTests {
    private static func decode<T: Decodable>(_ json: String) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: Data(json.utf8))
    }

    private static func task(_ id: String, request: String = "r-1", title: String? = nil, state: DomainV2.TaskState = .running) -> DomainV2.Task {
        DomainV2.Task(id: .init(rawValue: id), requestId: .init(rawValue: request), agentId: "s1", title: title ?? "Task \(id)",
                      state: state, revision: 1, started: true, reviewPrs: true, finalPresentation: nil, prId: nil,
                      dependencies: [], stack: nil, documents: [:], archived: false)
    }

    @Test func aTasksFleetRowTitleNeverRepeatsItsOwnNodesName() {
        #expect(Self.task("a").fleetRowTitle(nodeDisplayName: "alondra/other") == "Task a", "a real title passes through")
        let fallenBack = Self.task("b", title: "alondra/estados-por-tarea")
        #expect(fallenBack.fleetRowTitle(nodeDisplayName: "alondra/estados-por-tarea") == DomainV2.Task.mainTaskLabel,
                "the fallback title, repeated, reads as the node's main task instead")
    }

    @Test func aRequestTheOrchestratorHasNotSplitIsDefiningAndAClosedOneIsFinished() {
        let board = TaskBoard(requests: [
            DomainV2.Request(id: "open", body: "Do the thing", closed: false),
            DomainV2.Request(id: "withdrawn", body: "Never mind", closed: true),
            DomainV2.Request(id: "split", body: "Split it", closed: false),
        ], tasks: [Self.task("t-1", request: "split")])
        let defining = board.groups.first { $0.kind == .defining }
        #expect(defining?.requests.map(\.id) == ["open"])
        #expect(defining?.title == "Defining")
        #expect(board.groups.first { $0.kind == .task(.done) }?.requests.map(\.id) == ["withdrawn"])
        #expect(board.openCount == 2, "the running task and the request still being defined")
    }

    @Test func aTasksPrHandlingReadsLikeTheLaunchOption() throws {
        let spec: OptionSpec = try Self.decode(#"{"id":"pr","label":"PR handling","kind":"choice","builtin":true,"default":"review","values":[{"id":"review","label":"review each PR"},{"id":"auto_merge","label":"auto-merge"}]}"#)
        #expect(spec.values.map(spec.title(of:)) == [OptionSpec.prHandling(autoMerge: false), OptionSpec.prHandling(autoMerge: true)])
    }
}
