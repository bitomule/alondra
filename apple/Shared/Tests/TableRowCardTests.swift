import AlondraShared
import Foundation
import Testing

@Suite struct TableRowCardTests {
    struct Case: CustomTestStringConvertible, Sendable {
        let name: String
        let source: String
        let title: String
        let fields: [String]

        var testDescription: String { name }
    }

    static let cases: [Case] = [
        Case(name: "a body row", source: "| App | Price | Rating |\n|---|---|---|\n| Lumo | 3 € | 4.8 |", title: "Lumo", fields: ["Price: 3 €", "Rating: 4.8"]),
        Case(name: "an empty cell is left out", source: "| App | Price | Rating |\n|---|---|---|\n| Quill |  | 4.1 |", title: "Quill", fields: ["Rating: 4.1"]),
        Case(name: "inline Markdown is kept as text", source: "| Item | Note |\n|---|---|\n| **Bold** | `code` here |", title: "Bold", fields: ["Note: code here"]),
        Case(name: "a single column has only a title", source: "| Only |\n|---|\n| one |", title: "one", fields: []),
    ]

    @Test(arguments: cases)
    func aRowBecomesATitleAndItsFields(_ testCase: Case) throws {
        let rows = MarkdownParser.blocks(from: testCase.source).compactMap(\.tableRow)
        let row = try #require(rows.last)
        let card = TableRowCard(row)
        #expect(String(card.title.characters) == testCase.title)
        #expect(card.fields.map { "\(String($0.name.characters)): \(String($0.value.characters))" } == testCase.fields)
    }
}
