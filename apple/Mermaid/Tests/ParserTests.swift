@testable import AlondraMermaid
import Testing

@Suite struct ParserTests {
    @Test func corpusIsThere() {
        #expect(Corpus.valid.count >= 40)
        #expect(Corpus.invalid.count >= 10)
    }

    @Test(arguments: Corpus.valid)
    func fixtureParsesToItsTree(_ name: String) throws {
        let tree = Tree.dump(try Corpus.diagram(name))
        Corpus.record(tree, as: "\(name).tree")
        #expect(tree == Corpus.read("valid", name, "tree"))
    }

    @Test(arguments: Corpus.invalid)
    func invalidFixtureStopsAtItsLineAndColumn(_ name: String) throws {
        let expected = try #require(Corpus.read("invalid", name, "error")).trimmingCharacters(in: .whitespacesAndNewlines)
        var stopped: String?
        do {
            _ = try Mermaid.parse(try #require(Corpus.read("invalid", name, "mmd")))
        } catch let MermaidError.syntax(line, column, _) {
            stopped = "\(line):\(column)"
        } catch {
            Issue.record("\(name) failed with \(error), not a syntax error")
        }
        Corpus.record("\(stopped ?? "parsed")\n", as: "\(name).error")
        #expect(stopped == expected)
    }

    @Test func linkWithoutSpacesIsTwoNodesAndAnEdge() throws {
        guard case .flowchart(let chart) = try Mermaid.parse("graph LR\nA-->B") else {
            Issue.record("not a flowchart")
            return
        }
        #expect(chart.nodes.map(\.id) == ["A", "B"])
        #expect(chart.edges == [.init(from: "A", to: "B", label: nil, stroke: .solid, head: .arrow, tail: .none)])
    }

    @Test(arguments: ["pie", "gantt", "erDiagram", "mindmap", "xychart-beta", "journey", "gitGraph", "timeline", "somethingNew"])
    func unsupportedHeaderNamesItsKind(_ keyword: String) {
        #expect(throws: MermaidError.unsupported(kind: keyword)) {
            try Mermaid.parse("\(keyword)\n    title Something\n    \"A\" : 1\n")
        }
    }

    @Test func moreThanAHundredNodesIsRefusedBeforeLayout() {
        let source = "flowchart TD\n" + (1...100).map { "    n\($0) --> n\($0 + 1)" }.joined(separator: "\n")
        #expect(throws: MermaidError.tooLarge(nodes: 101)) { try Mermaid.parse(source) }
    }

    @Test func aHundredNodesStillParse() throws {
        let source = "flowchart TD\n" + (1...99).map { "    n\($0) --> n\($0 + 1)" }.joined(separator: "\n")
        #expect(try Mermaid.parse(source).nodeCount == 100)
    }

    @Test func aLongSequenceCountsItsMessages() {
        let source = "sequenceDiagram\n" + (1...99).map { _ in "    A->>B: ping" }.joined(separator: "\n")
        #expect(throws: MermaidError.tooLarge(nodes: 101)) { try Mermaid.parse(source) }
    }
}
