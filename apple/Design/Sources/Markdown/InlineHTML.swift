import Foundation

/// The HTML a README carries, rewritten as the Markdown it means, with the same rule as the
/// terminal reader: `<img>` becomes an image, `<a href>` a link, layout tags go away and `<br>` breaks
/// the line. Any other tag stays as written, and text that holds no HTML is left as it is.
public enum InlineHTML {
    public static func markdown(from source: String) -> String {
        guard source.contains("<") else { return source }
        let lines = source.components(separatedBy: "\n")
        var out: [String] = []
        var paragraph: [String] = []
        var fence: (marker: Character, length: Int)?
        var indentedCode = false
        var previousBlank = true

        func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            out.append(inline(paragraph.joined(separator: "\n")))
            paragraph = []
        }

        var index = 0
        while index < lines.count {
            let line = lines[index]
            let blank = line.allSatisfy(\.isWhitespace)
            defer { previousBlank = blank }
            if let open = fence {
                out.append(line)
                if closesFence(line, open) { fence = nil }
                index += 1
                continue
            }
            if let open = opensFence(line) {
                flushParagraph()
                fence = open
                out.append(line)
                index += 1
                continue
            }
            if indentedCode, blank || indent(of: line) >= 4 {
                out.append(line)
                index += 1
                continue
            }
            indentedCode = false
            if blank {
                flushParagraph()
                out.append(line)
                index += 1
                continue
            }
            if previousBlank, paragraph.isEmpty, indent(of: line) >= 4, !inList(out) {
                indentedCode = true
                out.append(line)
                index += 1
                continue
            }
            if let end = htmlBlockEnd(lines, at: index, interrupting: !paragraph.isEmpty) {
                flushParagraph()
                let block = lines[index...end].joined(separator: "\n")
                let prefix = String(line.prefix(while: { $0 == " " || $0 == "\t" }))
                let converted = self.block(block)
                if !converted.isEmpty {
                    if let last = out.last, !last.allSatisfy(\.isWhitespace) { out.append("") }
                    out.append(converted.map { prefix + $0 }.joined(separator: "\\\n"))
                    if end + 1 < lines.count, !lines[end + 1].allSatisfy(\.isWhitespace) { out.append("") }
                }
                index = end + 1
                continue
            }
            paragraph.append(line)
            index += 1
        }
        flushParagraph()
        return out.joined(separator: "\n")
    }

    // MARK: Blocks

    private static let blockTags: Set<String> = [
        "address", "article", "aside", "base", "basefont", "blockquote", "body", "caption", "center", "col",
        "colgroup", "dd", "details", "dialog", "dir", "div", "dl", "dt", "fieldset", "figcaption", "figure",
        "footer", "form", "frame", "frameset", "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hr", "html",
        "iframe", "legend", "li", "link", "main", "menu", "menuitem", "nav", "noframes", "ol", "optgroup", "option",
        "p", "param", "search", "section", "source", "summary", "table", "tbody", "td", "tfoot", "th", "thead",
        "title", "tr", "track", "ul",
    ]
    private static let rawTags: Set<String> = ["pre", "script", "style", "textarea"]
    private static let breakingTags: Set<String> = ["br", "p", "div", "center", "details", "summary", "h1", "h2", "h3", "h4", "h5", "h6"]
    private static let droppedTags: Set<String> = ["span", "picture", "source", "sub", "sup", "b", "i", "strong", "em"]

    /// The last line of the HTML block CommonMark starts at `index`, if one starts there.
    private static func htmlBlockEnd(_ lines: [String], at index: Int, interrupting: Bool) -> Int? {
        let line = lines[index]
        guard indent(of: line) < 4 else { return nil }
        let start = line.drop(while: { $0 == " " || $0 == "\t" })
        guard start.first == "<" else { return nil }
        if start.hasPrefix("<!--") {
            return lines[index...].firstIndex(where: { $0.contains("-->") }) ?? lines.count - 1
        }
        let opener = String(start)
        var cursor = opener.index(after: opener.startIndex)
        if cursor < opener.endIndex, opener[cursor] == "/" { cursor = opener.index(after: cursor) }
        let name = String(opener[cursor...].prefix(while: { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") })).lowercased()
        guard !name.isEmpty, !rawTags.contains(name) else { return nil }
        let after = opener[cursor...].dropFirst(name.count).first
        let opens: Bool
        if blockTags.contains(name) {
            opens = after == nil || after == ">" || after == "/" || after!.isWhitespace
        } else {
            let (pieces, rest) = Self.pieces(opener)
            let onlyTag = rest.isEmpty && pieces.count >= 1 && {
                guard case .tag = pieces[0] else { return false }
                return pieces.dropFirst().allSatisfy { if case .text(let t) = $0 { t.allSatisfy(\.isWhitespace) } else { false } }
            }()
            opens = !interrupting && onlyTag
        }
        guard opens else { return nil }
        return lines[index...].firstIndex(where: { $0.allSatisfy(\.isWhitespace) }).map { $0 - 1 } ?? lines.count - 1
    }

    /// An HTML block laid out the way a browser would: one line per `<br>` or paragraph, spaces collapsed.
    private static func block(_ source: String) -> [String] {
        var writer = Writer(escapesText: true)
        for piece in pieces(source).pieces {
            switch piece {
            case .text(let text):
                writer.text(collapse(text))
            case .comment:
                break
            case .tag(let tag):
                if breakingTags.contains(tag.name), !(tag.name == "br" && tag.closing) {
                    writer.lineBreak(always: tag.name == "br")
                } else {
                    writer.tag(tag)
                }
            }
        }
        return writer.lines
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    // MARK: Inline

    /// A paragraph's tags, leaving code spans and escaped characters alone.
    private static func inline(_ source: String) -> String {
        let c = Array(source)
        var writer = Writer(escapesText: false)
        var i = 0
        while i < c.count {
            switch c[i] {
            case "\\":
                writer.append(String(c[i..<min(i + 2, c.count)]))
                i += 2
            case "`":
                let ticks = c[i...].prefix(while: { $0 == "`" }).count
                let close = codeSpanEnd(c, from: i + ticks, ticks: ticks)
                writer.append(String(c[i..<(close ?? i + ticks)]))
                i = close ?? i + ticks
            case "<":
                if c[i...].starts(with: "<!--"), let close = find("-->", in: c, from: i + 4) {
                    i = close + 3
                } else if case .tag(let tag, let end) = readTag(c, at: i) {
                    i = end
                    if tag.name == "br", !tag.closing {
                        writer.append("\\\n")
                        while i < c.count, c[i] == " " || c[i] == "\t" { i += 1 }
                        if i < c.count, c[i] == "\n" { i += 1 }
                    } else if !breakingTags.contains(tag.name) {
                        writer.tag(tag)
                    }
                } else {
                    writer.append("<")
                    i += 1
                }
            default:
                writer.append(String(c[i]))
                i += 1
            }
        }
        return writer.lines.joined(separator: "\n")
    }

    private static func codeSpanEnd(_ c: [Character], from start: Int, ticks: Int) -> Int? {
        var i = start
        while i < c.count {
            guard c[i] == "`" else { i += 1; continue }
            let run = c[i...].prefix(while: { $0 == "`" }).count
            if run == ticks { return i + run }
            i += run
        }
        return nil
    }

    // MARK: Writing

    private struct Writer {
        let escapesText: Bool
        var lines: [String] = [""]
        var links: [String?] = []

        mutating func append(_ text: String) {
            lines[lines.count - 1] += text
        }

        mutating func text(_ text: String) {
            append(escapesText ? InlineHTML.escaped(text) : text)
        }

        mutating func lineBreak(always: Bool) {
            if always || !lines[lines.count - 1].allSatisfy(\.isWhitespace) {
                lines.append("")
            }
        }

        mutating func tag(_ tag: Tag) {
            switch tag.name {
            case "img" where !tag.closing:
                guard let source = tag.attribute("src"), !source.isEmpty else { return }
                append("![\(InlineHTML.escapedLabel(tag.attribute("alt") ?? ""))](\(InlineHTML.destination(source)))")
            case "img":
                return
            case "a" where tag.closing:
                guard let link = links.popLast() else { return append(tag.raw) }
                if let link { append("](\(InlineHTML.destination(link)))") }
            case "a":
                let href = tag.attribute("href").flatMap { $0.isEmpty ? nil : $0 }
                links.append(href)
                if href != nil { append("[") }
            default:
                if !droppedTags.contains(tag.name) { append(tag.raw) }
            }
        }
    }

    // MARK: Reading

    struct Tag {
        let name: String
        let closing: Bool
        let attributes: [(String, String)]
        let raw: String

        func attribute(_ key: String) -> String? {
            attributes.first(where: { $0.0 == key })?.1
        }
    }

    enum Piece {
        case text(String)
        case tag(Tag)
        case comment
    }

    /// The tags, comments and text of `source`, and what is left when it ends inside a tag.
    static func pieces(_ source: String) -> (pieces: [Piece], rest: String) {
        let characters = Array(source)
        var pieces: [Piece] = []
        var text = ""
        var index = 0
        func flush() {
            if !text.isEmpty { pieces.append(.text(text)) }
            text = ""
        }
        while index < characters.count {
            guard characters[index] == "<" else {
                text.append(characters[index])
                index += 1
                continue
            }
            if characters[index...].starts(with: "<!--") {
                guard let close = find("-->", in: characters, from: index + 4) else {
                    flush()
                    return (pieces, String(characters[index...]))
                }
                flush()
                pieces.append(.comment)
                index = close + 3
                continue
            }
            switch readTag(characters, at: index) {
            case .tag(let tag, let end):
                flush()
                pieces.append(.tag(tag))
                index = end
            case .incomplete:
                flush()
                return (pieces, String(characters[index...]))
            case .notATag:
                text.append("<")
                index += 1
            }
        }
        flush()
        return (pieces, "")
    }

    private enum Read {
        case tag(Tag, end: Int)
        case incomplete
        case notATag
    }

    private static func readTag(_ c: [Character], at start: Int) -> Read {
        var i = start + 1
        let closing = i < c.count && c[i] == "/"
        if closing { i += 1 }
        guard i < c.count, c[i].isASCII, c[i].isLetter else { return i >= c.count ? .incomplete : .notATag }
        let nameStart = i
        while i < c.count, c[i].isASCII, c[i].isLetter || c[i].isNumber || c[i] == "-" { i += 1 }
        let name = String(c[nameStart..<i]).lowercased()
        guard i < c.count else { return .incomplete }
        guard c[i].isWhitespace || c[i] == ">" || c[i] == "/" else { return .notATag }
        var attributes: [(String, String)] = []
        while true {
            while i < c.count, c[i].isWhitespace { i += 1 }
            guard i < c.count else { return .incomplete }
            if c[i] == ">" { break }
            if c[i] == "/" { i += 1; continue }
            let keyStart = i
            while i < c.count, !c[i].isWhitespace, !"=>/".contains(c[i]) { i += 1 }
            let key = String(c[keyStart..<i]).lowercased()
            var j = i
            while j < c.count, c[j].isWhitespace { j += 1 }
            guard j < c.count, c[j] == "=" else {
                attributes.append((key, ""))
                continue
            }
            j += 1
            while j < c.count, c[j].isWhitespace { j += 1 }
            guard j < c.count else { return .incomplete }
            let value: String
            if c[j] == "\"" || c[j] == "'" {
                guard let close = c[(j + 1)...].firstIndex(of: c[j]) else { return .incomplete }
                value = String(c[(j + 1)..<close])
                i = close + 1
            } else {
                let valueStart = j
                while j < c.count, !c[j].isWhitespace, c[j] != ">" { j += 1 }
                value = String(c[valueStart..<j])
                i = j
            }
            attributes.append((key, value))
        }
        let end = i + 1
        return .tag(Tag(name: name, closing: closing, attributes: attributes, raw: String(c[start..<end])), end: end)
    }

    private static func find(_ needle: String, in characters: [Character], from start: Int) -> Int? {
        let target = Array(needle)
        guard characters.count >= target.count else { return nil }
        var index = start
        while index + target.count <= characters.count {
            if Array(characters[index..<(index + target.count)]) == target { return index }
            index += 1
        }
        return nil
    }

    // MARK: Helpers

    private static func indent(of line: String) -> Int {
        var width = 0
        for character in line {
            switch character {
            case " ": width += 1
            case "\t": width += 4 - width % 4
            default: return width
            }
        }
        return width
    }

    private static func opensFence(_ line: String) -> (marker: Character, length: Int)? {
        let trimmed = line.drop(while: { $0 == " " || $0 == "\t" })
        guard let marker = trimmed.first, marker == "`" || marker == "~" else { return nil }
        let length = trimmed.prefix(while: { $0 == marker }).count
        guard length >= 3 else { return nil }
        if marker == "`", trimmed.dropFirst(length).contains("`") { return nil }
        return (marker, length)
    }

    private static func closesFence(_ line: String, _ open: (marker: Character, length: Int)) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        return trimmed.count >= open.length && trimmed.allSatisfy { $0 == open.marker }
    }

    private static func inList(_ out: [String]) -> Bool {
        guard let last = out.last(where: { !$0.allSatisfy(\.isWhitespace) }) else { return false }
        let trimmed = last.drop(while: { $0 == " " || $0 == "\t" })
        if indent(of: last) >= 2 { return true }
        if let first = trimmed.first, "-*+".contains(first), trimmed.dropFirst().first == " " { return true }
        let digits = trimmed.prefix(while: \.isNumber)
        return !digits.isEmpty && [".", ")"].contains(trimmed.dropFirst(digits.count).first)
    }

    private static func collapse(_ text: String) -> String {
        var out = ""
        for character in text {
            if character.isWhitespace, character != "\u{a0}" {
                if out.last != " " { out.append(" ") }
            } else {
                out.append(character)
            }
        }
        return out
    }

    private static let markdownPunctuation = Set("\\`*_[]<>#!|~+-.()")

    static func escaped(_ text: String) -> String {
        var out = ""
        for character in text {
            if markdownPunctuation.contains(character) { out.append("\\") }
            out.append(character)
        }
        return out
    }

    static func escapedLabel(_ text: String) -> String {
        var out = ""
        for character in text {
            if "\\[]".contains(character) { out.append("\\") }
            out.append(character)
        }
        return out
    }

    static func destination(_ url: String) -> String {
        var out = ""
        for character in url.trimmingCharacters(in: .whitespacesAndNewlines) {
            switch character {
            case " ": out += "%20"
            case "\n", "\t": continue
            case "<": out += "%3C"
            case ">": out += "%3E"
            case "(", ")", "\\": out += "\\\(character)"
            default: out.append(character)
            }
        }
        return out
    }
}
