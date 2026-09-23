import Foundation

public struct MarkdownBlock: Identifiable, Hashable, Sendable {
    public enum ListMarker: Hashable, Sendable {
        case bullet
        case number(String)
        case task(done: Bool)
    }

    public enum Kind: Hashable, Sendable {
        case heading(level: Int)
        case paragraph
        case listItem(marker: ListMarker, depth: Int)
        case code(language: String?)
        case quote
        case table(header: [String], rows: [[String]])
        case rule
    }

    public let id: Int
    public let kind: Kind
    public let text: String

    public var plainText: String {
        switch kind {
        case .code: return text
        case .rule: return ""
        case .table(let header, let rows):
            return ([header] + rows).map { $0.joined(separator: " | ") }.joined(separator: "\n")
        default:
            let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
            guard let attributed = try? AttributedString(markdown: text, options: options) else { return text }
            return String(attributed.characters)
        }
    }

    public static func inline(_ source: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        return (try? AttributedString(markdown: source, options: options)) ?? AttributedString(source)
    }

    public var isCommentable: Bool {
        if case .rule = kind { return false }
        return true
    }
}

public enum MarkdownParser {
    public static func blocks(from source: String) -> [MarkdownBlock] {
        var parser = Parser(lines: source.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n"))
        return parser.run()
    }

    private struct Parser {
        let lines: [String]
        var index = 0
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []

        init(lines: [String]) {
            self.lines = lines
        }

        mutating func run() -> [MarkdownBlock] {
            while index < lines.count {
                let line = lines[index]
                let trimmed = line.trimmingCharacters(in: .whitespaces)

                if trimmed.isEmpty {
                    flushParagraph()
                    index += 1
                } else if let fence = fenceMarker(trimmed) {
                    flushParagraph()
                    readCode(fence: fence, info: String(trimmed.dropFirst(fence.count)))
                } else if let heading = heading(trimmed) {
                    flushParagraph()
                    append(.heading(level: heading.level), heading.text)
                    index += 1
                } else if isRule(trimmed) {
                    flushParagraph()
                    append(.rule, "")
                    index += 1
                } else if trimmed.hasPrefix("|") {
                    flushParagraph()
                    readTable()
                } else if trimmed.hasPrefix(">") {
                    flushParagraph()
                    readQuote()
                } else if let item = listItem(line) {
                    flushParagraph()
                    readListItem(item)
                } else {
                    paragraph.append(trimmed)
                    index += 1
                }
            }
            flushParagraph()
            return blocks
        }

        mutating func append(_ kind: MarkdownBlock.Kind, _ text: String) {
            blocks.append(MarkdownBlock(id: blocks.count, kind: kind, text: text))
        }

        mutating func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            append(.paragraph, paragraph.joined(separator: "\n"))
            paragraph = []
        }

        func fenceMarker(_ trimmed: String) -> String? {
            if trimmed.hasPrefix("```") { return "```" }
            if trimmed.hasPrefix("~~~") { return "~~~" }
            return nil
        }

        mutating func readCode(fence: String, info: String) {
            index += 1
            var body: [String] = []
            while index < lines.count, !lines[index].trimmingCharacters(in: .whitespaces).hasPrefix(fence) {
                body.append(lines[index])
                index += 1
            }
            index += 1
            let language = info.trimmingCharacters(in: .whitespaces)
            append(.code(language: language.isEmpty ? nil : language), body.joined(separator: "\n"))
        }

        func heading(_ trimmed: String) -> (level: Int, text: String)? {
            let hashes = trimmed.prefix { $0 == "#" }.count
            guard (1...6).contains(hashes) else { return nil }
            let rest = trimmed.dropFirst(hashes)
            guard rest.isEmpty || rest.first == " " else { return nil }
            let text = rest.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "#")).trimmingCharacters(in: .whitespaces)
            return (hashes, text)
        }

        func isRule(_ trimmed: String) -> Bool {
            let compact = trimmed.replacingOccurrences(of: " ", with: "")
            guard compact.count >= 3, let first = compact.first, "-*_".contains(first) else { return false }
            return compact.allSatisfy { $0 == first }
        }

        mutating func readTable() {
            var rows: [[String]] = []
            while index < lines.count {
                let trimmed = lines[index].trimmingCharacters(in: .whitespaces)
                guard trimmed.hasPrefix("|") else { break }
                let cells = cellsOf(trimmed)
                let isSeparator = cells.allSatisfy { cell in
                    !cell.isEmpty && cell.allSatisfy { "-:".contains($0) }
                }
                if !isSeparator { rows.append(cells) }
                index += 1
            }
            guard let header = rows.first else { return }
            let width = rows.map(\.count).max() ?? header.count
            let padded = rows.map { $0 + Array(repeating: "", count: width - $0.count) }
            append(.table(header: padded[0], rows: Array(padded.dropFirst())), "")
        }

        func cellsOf(_ row: String) -> [String] {
            var body = Substring(row)
            if body.hasPrefix("|") { body = body.dropFirst() }
            if body.hasSuffix("|") { body = body.dropLast() }
            return body.split(separator: "|", omittingEmptySubsequences: false).map { $0.trimmingCharacters(in: .whitespaces) }
        }

        mutating func readQuote() {
            var body: [String] = []
            while index < lines.count {
                let trimmed = lines[index].trimmingCharacters(in: .whitespaces)
                guard trimmed.hasPrefix(">") else { break }
                body.append(String(trimmed.dropFirst()).trimmingCharacters(in: .whitespaces))
                index += 1
            }
            append(.quote, body.joined(separator: "\n"))
        }

        struct Item {
            let marker: MarkdownBlock.ListMarker
            let indent: Int
            let text: String
        }

        func listItem(_ line: String) -> Item? {
            let indent = line.prefix { $0 == " " || $0 == "\t" }.reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
            let rest = line.drop { $0 == " " || $0 == "\t" }
            let marker: MarkdownBlock.ListMarker
            let text: Substring
            if let first = rest.first, "-*+".contains(first), rest.dropFirst().first == " " {
                marker = .bullet
                text = rest.dropFirst(2)
            } else {
                let digits = rest.prefix { $0.isNumber }
                let after = rest.dropFirst(digits.count)
                guard !digits.isEmpty, digits.count < 10, let dot = after.first, dot == "." || dot == ")", after.dropFirst().first == " " else { return nil }
                marker = .number(String(digits))
                text = after.dropFirst(2)
            }
            let body = text.trimmingCharacters(in: .whitespaces)
            if case .bullet = marker, body.count >= 3, body.hasPrefix("[") {
                let state = body.dropFirst().first
                if body.dropFirst(2).first == "]", state == " " || state == "x" || state == "X" {
                    return Item(marker: .task(done: state != " "), indent: indent, text: String(body.dropFirst(3)).trimmingCharacters(in: .whitespaces))
                }
            }
            return Item(marker: marker, indent: indent, text: body)
        }

        mutating func readListItem(_ item: Item) {
            var text = [item.text]
            index += 1
            while index < lines.count {
                let line = lines[index]
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                let indent = line.prefix { $0 == " " || $0 == "\t" }.count
                guard !trimmed.isEmpty, indent > item.indent, listItem(line) == nil, fenceMarker(trimmed) == nil else { break }
                text.append(trimmed)
                index += 1
            }
            append(.listItem(marker: item.marker, depth: min(item.indent / 2, 4)), text.joined(separator: "\n"))
        }
    }
}
