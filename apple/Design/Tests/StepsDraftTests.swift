import AlondraContract
import AlondraDesign
import Testing

@Suite struct StepsDraftTests {
    static let text = "Given a paid order of 30 EUR\nWhen these amounts are refunded\n  | agent | amount |\n  | ana | 20 EUR |\nThen the second refund is refused\n  \"\"\"\n  Only 10 EUR are left to refund.\n  \"\"\"\nAnd the order keeps 10 EUR\n\nExamples:\n  | amount | reason |\n  | 0 EUR | nothing to refund |"

    @Test func stepsTablesTextBlocksAndExamplesAreReadAsRows() throws {
        let draft = try #require(StepsDraft(parsing: Self.text))
        #expect(draft.rows.map(\.keyword) == [.given, .when, .then, .and])
        #expect(draft.rows[1].table == [["agent", "amount"], ["ana", "20 EUR"]])
        #expect(draft.rows[2].doc == "Only 10 EUR are left to refund.")
        #expect(draft.examples == [["amount", "reason"], ["0 EUR", "nothing to refund"]])
    }

    @Test func writingTheRowsGivesTheTextBack() throws {
        let draft = try #require(StepsDraft(parsing: Self.text))
        #expect(draft.text == Self.text)
    }

    @Test func aCellKeepsItsPipesAndLineBreaks() throws {
        var draft = try #require(StepsDraft(parsing: "Given a thing"))
        draft.rows[0].table = [["a | b", "c\nd"], ["back\\slash", ""]]
        let again = try #require(StepsDraft(parsing: draft.text))
        #expect(again.rows[0].table == draft.rows[0].table)
    }

    @Test(arguments: ["Some words that are not a step", "Given a step\n  \"\"\"\n  never closed", "| a | b |"])
    func textThatIsNotMadeOfStepsIsNotRead(_ text: String) {
        #expect(StepsDraft(parsing: text) == nil)
    }

    @Test func aRowIsAddedRemovedAndMoved() throws {
        var draft = try #require(StepsDraft(parsing: "Given a\nWhen b\nThen c"))
        draft.add(.and)
        #expect(draft.rows.map(\.keyword) == [.given, .when, .then, .and])
        let second = draft.rows[1].id
        draft.move(second, by: -1)
        #expect(draft.rows.map(\.text) == ["b", "a", "c", ""])
        #expect(!draft.canMove(draft.rows[0].id, by: -1))
        draft.remove(second)
        #expect(draft.rows.map(\.text) == ["a", "c", ""])
        #expect(draft.text == "Given a\nThen c\nAnd")
    }

    @Test func andAndButJoinTheLaneBeforeThem() throws {
        let draft = try #require(StepsDraft(parsing: "Given a\nAnd b\nWhen c\nThen d\nBut e"))
        let lanes = draft.lanes()
        #expect(lanes.map(\.kind) == [.given, .when, .then])
        #expect(lanes.map { $0.steps.map(\.text) } == [["a", "b"], ["c"], ["d", "e"]])
    }

    @Test func aLaneKeepsTheWordsTheDaemonGaveIt() throws {
        let draft = try #require(StepsDraft(parsing: "Given a\nWhen b"))
        let known = [SheetLane(kind: .given, label: "Partiendo de", steps: []), SheetLane(kind: .when, label: "Cuando", steps: [])]
        #expect(draft.lanes(labelled: known).map(\.label) == ["Partiendo de", "Cuando"])
    }
}
