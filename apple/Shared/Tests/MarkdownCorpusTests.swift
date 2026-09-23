import AlondraShared
import Foundation
import Testing

@Suite struct MarkdownCorpusTests {
    /// Fixtures `MarkdownParser` still gets wrong. Remove a name when its fixture starts passing.
    static let knownBroken: Set<String> = [
        "card_apple_ads_steps",
        "card_blurcamera_pr_body",
        "code_in_list",
        "fence_long",
        "inline_mix",
        "nested_lists",
        "pipe_not_table",
        "quote_nested",
        "setext_headings",
        "table_align_code",
        "table_no_outer_pipes",
    ]

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
        func cell(_ text: String) -> String { String(MarkdownBlock.inline(text).characters) }
        return blocks.flatMap { block -> [String] in
            switch block.kind {
            case .heading(let level):
                return ["heading:\(level) 0 | \(escaped(block.plainText))"]
            case .paragraph:
                return ["para 0 | \(escaped(block.plainText))"]
            case .listItem(let marker, let depth):
                let symbol = switch marker {
                case .bullet: "-"
                case .number(let n): "\(n)."
                case .task(let done): done ? "[x]" : "[ ]"
                }
                return ["item \(depth) | \(symbol) \(escaped(block.plainText))"]
            case .code(let language):
                return ["code\(language.map { ":\($0)" } ?? "") 0 | \(escaped(block.text))"]
            case .quote:
                return ["quote 0 | \(escaped(block.plainText))"]
            case .table(let header, let rows):
                let head = "head:\(String(repeating: "l", count: header.count)) 0 | \(header.map(cell).joined(separator: " · "))"
                return [head] + rows.enumerated().map { "row \($0.offset + 1) | \($0.element.map(cell).joined(separator: " · "))" }
            case .rule:
                return ["rule 0 |"]
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
