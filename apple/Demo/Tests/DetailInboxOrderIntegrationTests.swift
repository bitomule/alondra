import AlondraShared
import AlondraWorkspaceFeature
import Foundation
import Testing

struct DetailInboxOrderIntegrationTests {
    @Test
    func nodeAndTaskDetailsKeepTheServerInboxOrder() throws {
        var fixture = V2Fixture()
        // The server's order, deliberately not the creation order; the question inside the PR
        // stays in its root and never appears twice.
        let newer = fixture.question("q2", title: "Newer question", order: 20)
        let older = fixture.question("q1", title: "Older question", order: 10)
        let pr = fixture.pullRequest(5, questions: ["q3"], order: 5)
        let newerDocument = fixture.documentReview("b", order: 50)
        let olderDocument = fixture.documentReview("a", order: 40)
        let presentation = ServiceTaskPresentation(workspace: fixture.workspace)
        let task = try #require(presentation.board.task(V2Fixture.task))
        let context = ServiceTaskContext(presentation: presentation, taskID: task.id)
        let expected = [newer.id, older.id, pr.id, newerDocument.id, olderDocument.id]
        #expect(presentation.pending(for: V2Fixture.agent).map(\.id) == expected)
        #expect(presentation.items(forTask: task).map(\.id) == expected)
        #expect(context.pending.map(\.id) == expected)
        let hidden = ServiceTaskPresentation(workspace: fixture.workspace, hiddenIDs: [newer.id, newerDocument.id])
        #expect(hidden.pending(for: V2Fixture.agent).map(\.id) == [older.id, pr.id, olderDocument.id])
        #expect(ServiceTaskContext(presentation: hidden, taskID: task.id).pending.map(\.id) == [older.id, pr.id, olderDocument.id])
    }
}
