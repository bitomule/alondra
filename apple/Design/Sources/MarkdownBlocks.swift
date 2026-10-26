import Foundation
import Synchronization

public struct MarkdownBlock: Identifiable, Hashable, Sendable {
    public enum ListMarker: Hashable, Sendable {
        case bullet
        case number(String)
        case task(done: Bool)
    }

    public enum ColumnAlignment: Hashable, Sendable {
        case leading
        case center
        case trailing
    }

    public struct TableRow: Hashable, Sendable {
        public let index: Int
        public let cells: [AttributedString]
        public let header: [AttributedString]
        public let alignments: [ColumnAlignment]

        public var isHeader: Bool { index == 0 }

        public func alignment(of column: Int) -> ColumnAlignment {
            alignments.indices.contains(column) ? alignments[column] : .leading
        }
    }

    public enum Kind: Hashable, Sendable {
        case heading(level: Int)
        case paragraph
        case listItem(marker: ListMarker)
        case code(language: String?)
        case quote
        case tableRow(TableRow)
        case rule
        case image(url: URL, alt: String)
    }

    public let id: Int
    public let kind: Kind
    public let depth: Int
    public let content: AttributedString
    public let plainText: String
    public let anchor: String

    public static func inline(_ source: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        return (try? AttributedString(markdown: source, options: options)) ?? AttributedString(source)
    }

    /// Markdown flattened to one line for a field that never grows into blocks: each block's text,
    /// with its inline styles and without its heading or list marker, joined by « · ».
    public static func line(_ source: String) -> AttributedString {
        var line = AttributedString()
        for block in MarkdownParser.blocks(from: source) where block.kind != .rule && !block.content.characters.isEmpty {
            if !line.characters.isEmpty { line.append(AttributedString(" · ")) }
            line.append(block.content)
        }
        return line
    }

    public var isCommentable: Bool {
        if case .rule = kind { return false }
        return true
    }

    public var opensLinks: Bool {
        content.runs[\.link].contains { link, _ in link != nil }
    }

    public var tableRow: TableRow? {
        if case .tableRow(let row) = kind { return row }
        return nil
    }

    static func name(of kind: Kind) -> String {
        switch kind {
        case .heading: "heading"
        case .paragraph: "paragraph"
        case .listItem: "item"
        case .code: "code"
        case .quote: "quote"
        case .tableRow: "row"
        case .rule: "rule"
        case .image: "image"
        }
    }
}

public enum MarkdownSegment: Identifiable, Hashable, Sendable {
    case block(MarkdownBlock)
    case table([MarkdownBlock])

    public var id: Int {
        switch self {
        case .block(let block): block.id
        case .table(let rows): rows.first?.id ?? -1
        }
    }
}

public enum MarkdownParser {
    static let cache = MarkdownBlockCache(capacity: 256)

    public static func blocks(from source: String) -> [MarkdownBlock] {
        cache.blocks(for: source)
    }

    static func parse(_ markdown: String) -> [MarkdownBlock] {
        let source = InlineHTML.markdown(from: markdown)
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .full, failurePolicy: .returnPartiallyParsedIfPossible)
        guard let document = try? AttributedString(markdown: source, options: options) else {
            var builder = Builder()
            builder.append(.paragraph, depth: 0, content: AttributedString(source))
            return builder.blocks
        }
        var builder = Builder()
        for (intent, range) in document.runs[\.presentationIntent] {
            var content = AttributedString(document[range])
            content.presentationIntent = nil
            builder.add(content, intent: intent?.components ?? [])
        }
        builder.flushRow()
        return builder.blocks
    }

    public static func segments(_ blocks: some Sequence<MarkdownBlock>) -> [MarkdownSegment] {
        var segments: [MarkdownSegment] = []
        for block in blocks {
            guard let row = block.tableRow else {
                segments.append(.block(block))
                continue
            }
            if !row.isHeader, case .table(let rows) = segments.last {
                segments[segments.count - 1] = .table(rows + [block])
            } else {
                segments.append(.table([block]))
            }
        }
        return segments
    }

    public static func segments(from source: String) -> [MarkdownSegment] {
        segments(blocks(from: source))
    }

    static func task(_ content: AttributedString) -> (done: Bool, rest: AttributedString)? {
        let prefix = String(content.characters.prefix(4))
        let done: Bool
        switch prefix {
        case "[ ] ": done = false
        case "[x] ", "[X] ": done = true
        default: return nil
        }
        var rest = content
        rest.removeSubrange(rest.startIndex..<rest.index(rest.startIndex, offsetByCharacters: 4))
        return (done, rest)
    }

    static let imageMark = "📷 "

    static func loneImage(_ content: AttributedString) -> (url: URL, alt: String)? {
        var found: (url: URL, alt: String)?
        for (url, range) in content.runs[\.imageURL] {
            let text = String(content[range].characters)
            guard let url else {
                guard text.allSatisfy(\.isWhitespace) else { return nil }
                continue
            }
            guard found == nil else { return nil }
            found = (url, withoutPlaceholder(text))
        }
        return found
    }

    /// Images inside a sentence read as links behind a camera mark; the mark is never part of the text.
    static func markingImages(_ content: AttributedString) -> AttributedString {
        var marked = content
        for (url, range) in content.runs[\.imageURL].reversed() {
            guard let url else { continue }
            let offset = content.characters.distance(from: content.startIndex, to: range.lowerBound)
            let length = content.characters.distance(from: range.lowerBound, to: range.upperBound)
            let start = marked.index(marked.startIndex, offsetByCharacters: offset)
            var end = marked.index(start, offsetByCharacters: length)
            if withoutPlaceholder(String(marked[start..<end].characters)).isEmpty {
                let attributes = marked[start..<end].runs.first?.attributes ?? AttributeContainer()
                marked.replaceSubrange(start..<end, with: AttributedString(url.lastPathComponent, attributes: attributes))
                end = marked.index(start, offsetByCharacters: url.lastPathComponent.count)
            }
            if isWeb(url), !marked[start..<end].runs[\.link].contains(where: { link, _ in link != nil }) {
                marked[start..<end].link = url
            }
            marked[start..<end].imageURL = nil
            let attributes = marked[start..<end].runs.first?.attributes ?? AttributeContainer()
            marked.insert(AttributedString(imageMark, attributes: attributes), at: start)
        }
        return marked
    }

    /// What Foundation leaves for an image without alt text.
    static func withoutPlaceholder(_ text: String) -> String {
        text.replacingOccurrences(of: "\u{FFFC}", with: "").trimmingCharacters(in: .whitespaces)
    }

    public static func isWeb(_ url: URL) -> Bool {
        url.scheme?.lowercased() == "https"
    }

    static func hash(_ text: String) -> String {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in text.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x0000_0100_0000_01b3
        }
        return String(hash, radix: 16)
    }

    private struct PendingRow {
        let identity: Int
        let index: Int
        let depth: Int
        let alignments: [MarkdownBlock.ColumnAlignment]
        var cells: [AttributedString]
    }

    private struct Builder {
        var blocks: [MarkdownBlock] = []
        var seenItems: Set<Int> = []
        var occurrences: [String: Int] = [:]
        var header: [AttributedString] = []
        var row: PendingRow?

        mutating func append(_ kind: MarkdownBlock.Kind, depth: Int, content: AttributedString, plainText: String? = nil) {
            let plainText = plainText ?? String(content.characters)
            let base = "\(MarkdownBlock.name(of: kind)):\(MarkdownParser.hash(plainText))"
            let occurrence = occurrences[base, default: 0]
            occurrences[base] = occurrence + 1
            blocks.append(MarkdownBlock(id: blocks.count, kind: kind, depth: depth, content: content, plainText: plainText, anchor: "\(base):\(occurrence)"))
        }

        mutating func add(_ content: AttributedString, intent components: [PresentationIntent.IntentType]) {
            guard let leaf = components.first else {
                flushRow()
                if !content.characters.isEmpty { append(.paragraph, depth: 0, content: content) }
                return
            }
            let containers = components.dropFirst().filter(\.isContainer)
            let depth = containers.count

            if case .tableCell(let column) = leaf.kind {
                addCell(content, column: column, components: components, depth: depth)
                return
            }
            flushRow()

            switch leaf.kind {
            case .paragraph:
                let innermost = containers.first
                if let innermost, case .listItem(let ordinal) = innermost.kind, seenItems.insert(innermost.identity).inserted {
                    let list = components.firstIndex(of: innermost).map { components.index(after: $0) }
                    let ordered = list.map { components.indices.contains($0) && components[$0].kind == .orderedList } ?? false
                    var marker: MarkdownBlock.ListMarker = ordered ? .number(String(ordinal)) : .bullet
                    var text = content
                    if let task = MarkdownParser.task(content) {
                        marker = .task(done: task.done)
                        text = task.rest
                    }
                    appendMarkingImages(.listItem(marker: marker), depth: depth - 1, content: text)
                } else if let innermost, innermost.kind == .blockQuote {
                    appendMarkingImages(.quote, depth: depth - 1, content: content)
                } else {
                    markItemSeen(containers.first)
                    if let image = MarkdownParser.loneImage(content) {
                        append(.image(url: image.url, alt: image.alt), depth: depth, content: AttributedString(image.alt))
                    } else {
                        append(.paragraph, depth: depth, content: MarkdownParser.markingImages(content), plainText: MarkdownParser.withoutPlaceholder(String(content.characters)))
                    }
                }
            case .header(let level):
                markItemSeen(containers.first)
                appendMarkingImages(.heading(level: level), depth: depth, content: content)
            case .codeBlock(let hint):
                markItemSeen(containers.first)
                var code = content
                if code.characters.last == "\n" {
                    code.removeSubrange(code.index(beforeCharacter: code.endIndex)..<code.endIndex)
                }
                let language = hint?.trimmingCharacters(in: .whitespaces)
                append(.code(language: language?.isEmpty == false ? language : nil), depth: depth, content: code)
            case .thematicBreak:
                append(.rule, depth: depth, content: AttributedString())
            default:
                append(.paragraph, depth: depth, content: content)
            }
        }

        mutating func appendMarkingImages(_ kind: MarkdownBlock.Kind, depth: Int, content: AttributedString) {
            guard content.runs[\.imageURL].contains(where: { url, _ in url != nil }) else {
                return append(kind, depth: depth, content: content)
            }
            append(kind, depth: depth, content: MarkdownParser.markingImages(content),
                   plainText: MarkdownParser.withoutPlaceholder(String(content.characters)))
        }

        mutating func markItemSeen(_ container: PresentationIntent.IntentType?) {
            guard let container, case .listItem = container.kind else { return }
            seenItems.insert(container.identity)
        }

        mutating func addCell(_ content: AttributedString, column: Int, components: [PresentationIntent.IntentType], depth: Int) {
            guard let rowIntent = components.dropFirst().first,
                  let table = components.first(where: { if case .table = $0.kind { true } else { false } }),
                  case .table(let columns) = table.kind
            else { return }
            if row?.identity != rowIntent.identity {
                flushRow()
                let index = if case .tableRow(let index) = rowIntent.kind { index } else { 0 }
                let alignments = columns.map { column -> MarkdownBlock.ColumnAlignment in
                    switch column.alignment {
                    case .center: .center
                    case .right: .trailing
                    default: .leading
                    }
                }
                row = PendingRow(identity: rowIntent.identity, index: index, depth: depth, alignments: alignments, cells: Array(repeating: AttributedString(), count: columns.count))
            }
            guard var pending = row else { return }
            if column >= pending.cells.count {
                pending.cells += Array(repeating: AttributedString(), count: column - pending.cells.count + 1)
            }
            pending.cells[column] = content
            row = pending
        }

        mutating func flushRow() {
            guard let pending = row else { return }
            row = nil
            if pending.index == 0 { header = pending.cells }
            let tableRow = MarkdownBlock.TableRow(index: pending.index, cells: pending.cells, header: header, alignments: pending.alignments)
            let content = pending.cells.dropFirst().reduce(into: pending.cells.first ?? AttributedString()) { $0 += AttributedString(" | ") + $1 }
            append(.tableRow(tableRow), depth: pending.depth, content: content)
        }
    }
}

private extension PresentationIntent.IntentType {
    var isContainer: Bool {
        switch kind {
        case .listItem, .blockQuote: true
        default: false
        }
    }
}

public final class MarkdownBlockCache: Sendable {
    private struct Entry {
        let blocks: [MarkdownBlock]
        var used: UInt64
    }

    private struct State {
        var entries: [String: Entry] = [:]
        var clock: UInt64 = 0
    }

    public let capacity: Int
    private let state = Mutex(State())

    public init(capacity: Int) {
        self.capacity = max(capacity, 1)
    }

    public var count: Int {
        state.withLock { $0.entries.count }
    }

    public func contains(_ source: String) -> Bool {
        state.withLock { $0.entries[source] != nil }
    }

    public func blocks(for source: String) -> [MarkdownBlock] {
        let cached = state.withLock { state -> [MarkdownBlock]? in
            state.clock += 1
            state.entries[source]?.used = state.clock
            return state.entries[source]?.blocks
        }
        if let cached { return cached }
        let blocks = MarkdownParser.parse(source)
        state.withLock { state in
            state.clock += 1
            state.entries[source] = Entry(blocks: blocks, used: state.clock)
            if state.entries.count > capacity, let oldest = state.entries.min(by: { $0.value.used < $1.value.used })?.key {
                state.entries[oldest] = nil
            }
        }
        return blocks
    }
}
