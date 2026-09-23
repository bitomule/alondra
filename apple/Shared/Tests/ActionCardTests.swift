import AlondraShared
import Foundation
import Testing

@Suite struct ActionCardTests {
    private func question(_ extra: String) throws -> Question {
        let json = """
        {"id":1,"sessionId":"s1","title":"Get the key","detail":"It unblocks the release.","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":0\(extra)}
        """
        return try JSONDecoder().decode(Question.self, from: Data(json.utf8))
    }

    @Test func anActionReadsItsStepsNumberedBeforeTheDetail() throws {
        let q = try question(#","action":true,"steps":["Open App Store Connect","Copy the key"]"#)
        #expect(q.isAction)
        #expect(q.readableDetail == "1. Open App Store Connect\n2. Copy the key\n\nIt unblocks the release.")
    }

    @Test func aQuestionFromAnOlderServerStillDecodesAndIsNotAnAction() throws {
        let q = try question("")
        #expect(!q.isAction)
        #expect(q.readableDetail == "It unblocks the release.")
    }

    @Test func theActionOptionsKeepTheLabelsTheServerMatches() {
        #expect(AnswerOption.done == "Done")
        #expect(AnswerOption.cannot == "I can't / explain")
    }
}
