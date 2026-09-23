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

    @Test func aCardFromTheTerminalCarriesTheOpeningOfTheMission() throws {
        let long = String(repeating: "word ", count: 80)
        let q = try question(",\"mission\":\"Ship Lumo\\n\\n4.5 \(long)\"")
        let excerpt = try #require(InboxItem.question(q).missionExcerpt)
        #expect(excerpt.hasPrefix("Ship Lumo 4.5 word"))
        #expect(excerpt.hasSuffix("…"))
        #expect(InboxItem.question(try question("")).missionExcerpt == nil)
    }
}
