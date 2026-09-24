import AlondraShared
import Foundation
import Testing

@Suite struct MarkdownCorpusTests {
    /// Fixtures `MarkdownParser` still gets wrong. Remove a name when its fixture starts passing.
    static let knownBroken: Set<String> = []

    struct Fixture: CustomTestStringConvertible {
        let name: String
        let markdown: String
        let blocks: String

        var testDescription: String { name }
    }

    static var directory: URL {
        let environment = ProcessInfo.processInfo.environment
        if let root = environment["TEST_SRCDIR"] {
            return URL(fileURLWithPath: root)
                .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("fixtures/markdown")
        }
        return URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("../../../fixtures/markdown")
            .standardizedFileURL
    }

    static let fixtures: [Fixture] = {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
        return names.filter { $0.hasSuffix(".md") }.sorted().compactMap { file in
            let name = String(file.dropLast(3))
            guard let markdown = try? String(contentsOf: directory.appendingPathComponent(file), encoding: .utf8),
                  let blocks = try? String(contentsOf: directory.appendingPathComponent("\(name).blocks"), encoding: .utf8)
            else { return nil }
            return Fixture(name: name, markdown: markdown, blocks: blocks)
        }
    }()

    static func outline(_ blocks: [MarkdownBlock]) -> [String] {
        func escaped(_ text: String) -> String { text.replacingOccurrences(of: "\n", with: "\\n") }
        return blocks.map { block -> String in
            let text = escaped(block.plainText)
            switch block.kind {
            case .heading(let level):
                return "heading:\(level) \(block.depth) | \(text)"
            case .paragraph:
                return "para \(block.depth) | \(text)"
            case .listItem(let marker):
                let symbol = switch marker {
                case .bullet: "-"
                case .number(let n): "\(n)."
                case .task(let done): done ? "[x]" : "[ ]"
                }
                return "item \(block.depth) | \(symbol) \(text)"
            case .code(let language):
                return "code\(language.map { ":\($0)" } ?? "") \(block.depth) | \(text)"
            case .quote:
                return "quote \(block.depth) | \(text)"
            case .tableRow(let row):
                let cells = row.cells.map { escaped(String($0.characters)) }.joined(separator: " · ")
                guard row.isHeader else { return "row \(row.index) | \(cells)" }
                let alignments = row.alignments.map { alignment in
                    switch alignment {
                    case .leading: "l"
                    case .center: "c"
                    case .trailing: "r"
                    }
                }
                return "head:\(alignments.joined()) \(block.depth) | \(cells)"
            case .rule:
                return "rule \(block.depth) |"
            }
        }
        .map { $0.trimmingCharacters(in: .whitespaces) }
    }

    @Test func theCorpusIsThere() {
        #expect(Self.fixtures.count >= 16, "found \(Self.fixtures.count) fixtures in \(Self.directory.path)")
        #expect(Self.knownBroken.isSubset(of: Set(Self.fixtures.map(\.name))))
    }

    @Test(arguments: fixtures)
    func parsesToItsOutlineUnlessKnownBroken(_ fixture: Fixture) {
        let expected = fixture.blocks.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
        let actual = Self.outline(MarkdownParser.blocks(from: fixture.markdown))
        if Self.knownBroken.contains(fixture.name) {
            #expect(actual != expected, "\(fixture.name) passes now: remove it from knownBroken")
        } else {
            #expect(actual == expected)
        }
    }
}
