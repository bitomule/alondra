import AlondraShared
import Foundation
import Testing

@Suite struct TaskPresentationTests {
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

    private static func asked(_ id: String, tasks: [String], state: String = "in_progress", synthetic: Bool = false, created: Int64 = 1, title: String? = nil, closedAt: Int64? = nil) throws -> WorkRequest {
        let ids = tasks.map { #""\#($0)""# }.joined(separator: ",")
        let named = title.map { #","title":"\#($0)""# } ?? ""
        let closed = closedAt.map { #","closed":{"at":\#($0)}"# } ?? ""
        return try decode(#"{"id":"\#(id)","text":"Do the thing","created_at":\#(created),"state":"\#(state)","synthetic":\#(synthetic),"tasks":[\#(ids)]\#(named)\#(closed)}"#)
    }

    @Test func aTasksFleetRowTitleNeverRepeatsItsOwnNodesName() throws {
        let named = try Self.task("a", created: 1)
        #expect(named.fleetRowTitle(nodeDisplayName: "alondra/other") == "Task a", "a real title passes through")
        let fallenBack: WorkTask = try Self.decode(
            #"{"id":"t-b","request":"r-1","title":"alondra/estados-por-tarea","depends_on":[],"options":{"auto_merge":false},"assignee":{"session_id":"s1"},"prs":[],"documents":[],"blocker":null,"notices":[],"state":"running","state_since":1,"created_at":1}"#
        )
        #expect(fallenBack.fleetRowTitle(nodeDisplayName: "alondra/estados-por-tarea") == WorkTask.mainTaskLabel, "the fallback title, repeated, reads as the node's main task instead")
    }

    @Test func aRequestTheOrchestratorLeftDefiningSaysItHasNoTasks() throws {
        let now = Date(timeIntervalSince1970: 10_000)
        let sent = { (minutesAgo: Int64) throws -> WorkRequest in try Self.asked("r", tasks: [], state: "defining", created: 10_000_000 - minutesAgo * 60_000) }
        #expect(try sent(1).isLeftDefining(now: now) == false)
        #expect(try sent(1).definingLabel(now: now) == "defining")
        #expect(try sent(11).isLeftDefining(now: now))
        #expect(try sent(11).definingLabel(now: now) == "no tasks")
    }

    @Test func aTasksPrHandlingReadsLikeTheLaunchOption() throws {
        let spec: OptionSpec = try Self.decode(#"{"id":"pr","label":"PR handling","kind":"choice","builtin":true,"default":"review","values":[{"id":"review","label":"review each PR"},{"id":"auto_merge","label":"auto-merge"}]}"#)
        #expect(spec.values.map(spec.title(of:)) == [OptionSpec.prHandling(autoMerge: false), OptionSpec.prHandling(autoMerge: true)])
    }
}
