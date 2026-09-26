import AlondraShared
import AlondraWorkspaceFeature
import Foundation
import Testing

struct DetailInboxOrderIntegrationTests {
    @Test
    func nodeAndTaskDetailsKeepOriginalMixedInboxOrder() throws {
        // Deliberately unsorted transport data; attached PR context must not appear twice.
        let json = #"""
        {"tick":1,"nodes":[],"orchestrator_messages":[],
         "machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false},
         "tasks":[{"id":"t1","request":"request1","title":"Ship","state":"running","state_since":1,"created_at":1}],
         "open_questions":[
          {"id":1,"session_id":"s1","task":"t1","title":"Older question","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":10},
          {"id":2,"session_id":"s1","task":"t1","title":"Newer question","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":20},
          {"id":3,"session_id":"s1","task":"t1","pr_id":5,"title":"Inside PR","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":30}],
         "prs":[{"id":5,"task":"t1","session_id":"s1","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","opened_at":5}],
         "news":[
          {"id":7,"task":"t1","session_id":"s1","title":"Older document","links":[],"attachments":[],"kind":"document","created_at":40},
          {"id":8,"task":"t1","session_id":"s1","title":"Newer document","links":[],"attachments":[],"kind":"document","created_at":50},
          {"id":9,"task":"t1","session_id":"s1","pr_id":5,"title":"Inside PR","links":[],"attachments":[],"kind":"document","created_at":60}]}
        """#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let overview = try decoder.decode(Overview.self, from: Data(json.utf8))
        let presentation = ServiceTaskPresentation(overview: overview)
        let task = try #require(presentation.board.task("t1"))
        let context = ServiceTaskContext(board: presentation.board, overview: overview, taskID: task.id)
        let expected = ["q2", "q1", "p5", "r8", "r7"]
        #expect(presentation.pending(for: "s1").map(\.id) == expected)
        #expect(presentation.items(forTask: task).map(\.id) == expected)
        #expect(context.pending.map(\.id) == expected)
        let hidden = ServiceTaskPresentation(overview: overview, hiddenIDs: ["q2", "r8"])
        #expect(hidden.pending(for: "s1").map(\.id) == ["q1", "p5", "r7"])
    }
}
