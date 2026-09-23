import SwiftUI

struct MarkdownBlock: Identifiable, Hashable, Sendable {
    enum Kind: Hashable, Sendable {
        case heading(Int)
        case paragraph
        case bullet(indent: Int)
        case numbered(String, indent: Int)
        case task(done: Bool, indent: Int)
        case quote
        case code(language: String?)
        case table
        case rule
    }

    let id: Int
    let kind: Kind
    let text: String

    var isCommentable: Bool { kind != .rule }

    var plainText: String {
        switch kind {
        case .code, .table: text
        default: String(Self.inline(text).characters)
        }
    }

    static func inline(_ source: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        return (try? AttributedString(markdown: source, options: options)) ?? AttributedString(source)
    }

    static func parse(_ source: String) -> [MarkdownBlock] {
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []
        var quote: [String] = []
        var table: [String] = []
        var code: [String]?
        var codeLanguage: String?

        func add(_ kind: Kind, _ text: String) {
            blocks.append(MarkdownBlock(id: blocks.count, kind: kind, text: text))
        }
        func flush() {
            if !paragraph.isEmpty { add(.paragraph, paragraph.joined(separator: " ")) }
            if !quote.isEmpty { add(.quote, quote.joined(separator: " ")) }
            if !table.isEmpty { add(.table, table.joined(separator: "\n")) }
            paragraph = []
            quote = []
            table = []
        }

        for rawLine in source.components(separatedBy: .newlines) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if var lines = code {
                if line.hasPrefix("```") || line.hasPrefix("~~~") {
                    add(.code(language: codeLanguage), lines.joined(separator: "\n"))
                    code = nil
                } else {
                    lines.append(rawLine)
                    code = lines
                }
                continue
            }
            if line.hasPrefix("```") || line.hasPrefix("~~~") {
                flush()
                let language = line.dropFirst(3).trimmingCharacters(in: .whitespaces)
                codeLanguage = language.isEmpty ? nil : language
                code = []
                continue
            }
            if line.isEmpty {
                flush()
                continue
            }
            let indent = (rawLine.prefix { $0 == " " || $0 == "\t" }.count) / 2
            if let heading = headingLevel(line) {
                flush()
                add(.heading(heading), String(line.drop { $0 == "#" }).trimmingCharacters(in: .whitespaces))
            } else if line.allSatisfy({ "-*_".contains($0) }) && line.count >= 3 && Set(line).count == 1 {
                flush()
                add(.rule, "")
            } else if line.hasPrefix(">") {
                if quote.isEmpty { flush() }
                quote.append(line.drop { $0 == ">" }.trimmingCharacters(in: .whitespaces))
            } else if line.hasPrefix("|") {
                if table.isEmpty { flush() }
                table.append(line)
            } else if let item = listItem(line) {
                flush()
                switch item {
                case .bullet(let text):
                    if let task = taskItem(text) {
                        add(.task(done: task.done, indent: indent), task.text)
                    } else {
                        add(.bullet(indent: indent), text)
                    }
                case .numbered(let marker, let text):
                    add(.numbered(marker, indent: indent), text)
                }
            } else {
                if !quote.isEmpty || !table.isEmpty { flush() }
                paragraph.append(line)
            }
        }
        if let lines = code { add(.code(language: codeLanguage), lines.joined(separator: "\n")) }
        flush()
        return blocks
    }

    private static func headingLevel(_ line: String) -> Int? {
        let hashes = line.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes), line.dropFirst(hashes).first == " " else { return nil }
        return hashes
    }

    private enum ListItem {
        case bullet(String)
        case numbered(String, String)
    }

    private static func listItem(_ line: String) -> ListItem? {
        if let first = line.first, "-*+".contains(first), line.dropFirst().first == " " {
            return .bullet(String(line.dropFirst(2)))
        }
        let digits = line.prefix { $0.isNumber }
        guard !digits.isEmpty, digits.count < 4 else { return nil }
        let rest = line.dropFirst(digits.count)
        guard let marker = rest.first, ".)".contains(marker), rest.dropFirst().first == " " else { return nil }
        return .numbered(String(digits), String(rest.dropFirst(2)))
    }

    private static func taskItem(_ text: String) -> (done: Bool, text: String)? {
        let lower = text.lowercased()
        if lower.hasPrefix("[ ] ") { return (false, String(text.dropFirst(4))) }
        if lower.hasPrefix("[x] ") { return (true, String(text.dropFirst(4))) }
        return nil
    }
}

struct MarkdownBlockView: View {
    let block: MarkdownBlock

    var body: some View {
        switch block.kind {
        case .heading(let level):
            Text(MarkdownBlock.inline(block.text))
                .font(headingFont(level))
                .foregroundStyle(Palette.ink)
                .padding(.top, level <= 2 ? 10 : 4)
                .accessibilityAddTraits(.isHeader)
        case .paragraph:
            Text(MarkdownBlock.inline(block.text))
                .font(.body)
                .lineSpacing(3)
        case .bullet(let indent):
            listRow(indent: indent) {
                Circle().fill(Palette.dawn).frame(width: 6, height: 6).padding(.top, 8)
            }
        case .numbered(let marker, let indent):
            listRow(indent: indent) {
                Text("\(marker).")
                    .font(.body.monospacedDigit().weight(.semibold))
                    .foregroundStyle(Palette.dawn)
            }
        case .task(let done, let indent):
            listRow(indent: indent) {
                Image(systemName: done ? "checkmark.square.fill" : "square")
                    .foregroundStyle(done ? Palette.sage : .secondary)
            }
        case .quote:
            HStack(alignment: .top, spacing: 12) {
                Capsule().fill(Palette.slate.opacity(0.5)).frame(width: 3)
                Text(MarkdownBlock.inline(block.text))
                    .font(.body.italic())
                    .foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .code(let language):
            VStack(alignment: .leading, spacing: 6) {
                if let language {
                    Text(language.uppercased())
                        .font(.caption2.weight(.bold))
                        .kerning(0.8)
                        .foregroundStyle(.tertiary)
                }
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(block.text)
                        .font(.mono(.callout))
                        .fixedSize()
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.night.opacity(0.7), in: .rect(cornerRadius: 12))
        case .table:
            TableBlock(source: block.text)
        case .rule:
            Rectangle().fill(Palette.hairline).frame(height: 1).padding(.vertical, 6)
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .display(.title)
        case 2: .display(.title2)
        case 3: .display(.title3)
        default: .headline
        }
    }

    private func listRow<Marker: View>(indent: Int, @ViewBuilder marker: () -> Marker) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            marker()
                .frame(minWidth: 18, alignment: .leading)
            Text(MarkdownBlock.inline(block.text))
                .font(.body)
                .lineSpacing(3)
        }
        .padding(.leading, CGFloat(indent) * 18)
    }
}

private struct TableBlock: View {
    let rows: [[String]]

    init(source: String) {
        rows = source.components(separatedBy: "\n")
            .map { line in
                line.trimmingCharacters(in: .whitespaces)
                    .trimmingCharacters(in: CharacterSet(charactersIn: "|"))
                    .components(separatedBy: "|")
                    .map { $0.trimmingCharacters(in: .whitespaces) }
            }
            .filter { row in !row.allSatisfy { cell in !cell.isEmpty && cell.allSatisfy { "-:".contains($0) } } }
    }

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 8) {
                ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                            Text(MarkdownBlock.inline(cell))
                                .font(index == 0 ? .subheadline.weight(.semibold) : .subheadline)
                                .frame(maxWidth: 260, alignment: .leading)
                        }
                    }
                    if index == 0 {
                        Divider().gridCellUnsizedAxes(.horizontal)
                    }
                }
            }
            .padding(14)
        }
        .background(Palette.night.opacity(0.5), in: .rect(cornerRadius: 12))
    }
}
