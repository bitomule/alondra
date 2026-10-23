import Foundation

/// The block's lines with their 1-based numbers, without the front matter and `%%` comments.
struct Document {
    struct Line {
        let number: Int
        let characters: [Character]

        var isBlank: Bool { characters.allSatisfy(\.isWhitespace) }
    }

    let lines: [Line]
    let frontMatterTitle: String?

    init(_ source: String) {
        let raw = source.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false)
        var lines: [Line] = []
        var title: String?
        var index = 0
        if let first = raw.firstIndex(where: { !$0.trimmingCharacters(in: .whitespaces).isEmpty }),
           raw[first].trimmingCharacters(in: .whitespaces) == "---",
           let close = raw[(first + 1)...].firstIndex(where: { $0.trimmingCharacters(in: .whitespaces) == "---" }) {
            for entry in raw[(first + 1)..<close] {
                let text = entry.trimmingCharacters(in: .whitespaces)
                if text.hasPrefix("title:") {
                    title = Label.clean(String(text.dropFirst("title:".count)))
                }
            }
            index = close + 1
        }
        while index < raw.count {
            let text = raw[index]
            if !text.trimmingCharacters(in: .whitespaces).hasPrefix("%%") {
                lines.append(Line(number: index + 1, characters: Array(text)))
            }
            index += 1
        }
        self.lines = lines
        frontMatterTitle = title.flatMap { $0.isEmpty ? nil : $0 }
    }
}

/// Reads one line left to right; every error it raises carries the line and column it stopped at.
struct Cursor {
    let line: Int
    let characters: [Character]
    var index: Int

    init(_ line: Document.Line, from index: Int = 0) {
        self.line = line.number
        characters = line.characters
        self.index = index
    }

    var isAtEnd: Bool { index >= characters.count }

    func peek(_ offset: Int = 0) -> Character? {
        let position = index + offset
        return position < characters.count && position >= 0 ? characters[position] : nil
    }

    func starts(with text: String) -> Bool {
        var position = index
        for character in text {
            guard position < characters.count, characters[position] == character else { return false }
            position += 1
        }
        return true
    }

    /// `keyword` followed by whitespace or the end of the line.
    func starts(withWord keyword: String, caseInsensitive: Bool = false) -> Bool {
        let length = keyword.count
        guard index + length <= characters.count else { return false }
        let word = String(characters[index..<(index + length)])
        let matches = caseInsensitive ? word.lowercased() == keyword.lowercased() : word == keyword
        guard matches else { return false }
        return index + length == characters.count || characters[index + length].isWhitespace
    }

    mutating func consume(_ text: String) -> Bool {
        guard starts(with: text) else { return false }
        index += text.count
        return true
    }

    mutating func skipSpaces() {
        while let character = peek(), character.isWhitespace { index += 1 }
    }

    mutating func read(while predicate: (Character) -> Bool) -> String {
        let start = index
        while let character = peek(), predicate(character) { index += 1 }
        return String(characters[start..<index])
    }

    mutating func rest() -> String {
        let text = String(characters[min(index, characters.count)...])
        index = characters.count
        return text
    }

    /// The text up to `terminator`, which is consumed; nil (and nothing consumed) when it never comes.
    mutating func read(until terminator: String) -> String? {
        let start = index
        while !isAtEnd {
            if starts(with: terminator) {
                let text = String(characters[start..<index])
                index += terminator.count
                return text
            }
            index += 1
        }
        index = start
        return nil
    }

    func error(_ expected: String, at position: Int? = nil) -> MermaidError {
        .syntax(line: line, column: (position ?? index) + 1, expected: expected)
    }

    mutating func expectEnd() throws(MermaidError) {
        skipSpaces()
        if !isAtEnd { throw error("end of line") }
    }
}

enum Identifier {
    static func isStart(_ character: Character) -> Bool {
        character.isLetter || character.isNumber || character == "_"
    }

    static func isBody(_ character: Character) -> Bool {
        isStart(character)
    }

    /// A word of letters, digits, `_` and `-`, where a `-` that starts a link (`--`, `-.`, `->`)
    /// ends it: `A-->B` is `A`, a link and `B`; `my-node` is one id.
    static func read(_ cursor: inout Cursor) -> String? {
        guard let first = cursor.peek(), isStart(first) else { return nil }
        let start = cursor.index
        while let character = cursor.peek() {
            if isBody(character) {
                cursor.index += 1
            } else if character == "-", let next = cursor.peek(1), isBody(next) {
                cursor.index += 1
            } else {
                break
            }
        }
        return String(cursor.characters[start..<cursor.index])
    }
}

/// Text as Mermaid writes it in a label, turned into what is drawn.
enum Label {
    private static let entities: [(String, String)] = [
        ("#quot;", "\""), ("#amp;", "&"), ("#lt;", "<"), ("#gt;", ">"), ("#35;", "#"), ("#59;", ";"),
        ("#nbsp;", "\u{00A0}"),
    ]

    static func clean(_ raw: String) -> String {
        var text = raw.trimmingCharacters(in: .whitespaces)
        if text.count >= 2, text.hasPrefix("\""), text.hasSuffix("\"") {
            text = String(text.dropFirst().dropLast())
        }
        if text.count >= 2, text.hasPrefix("`"), text.hasSuffix("`") {
            text = String(text.dropFirst().dropLast())
        }
        for breakTag in ["<br/>", "<br />", "<br>", "<BR>", "<BR/>", "\\n"] {
            text = text.replacingOccurrences(of: breakTag, with: "\n")
        }
        for (entity, value) in entities {
            text = text.replacingOccurrences(of: entity, with: value)
        }
        return text
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .joined(separator: "\n")
    }

    /// `~T~` generics as `<T>`.
    static func generics(_ raw: String) -> String {
        var result = ""
        var open = false
        for character in raw {
            if character == "~" {
                result.append(open ? ">" : "<")
                open.toggle()
            } else {
                result.append(character)
            }
        }
        return result
    }
}

/// `accTitle: …`, `accDescr: …` and the multi-line `accDescr { … }`, the same in every diagram.
struct AccessibilityReader {
    var accessibility = Accessibility()

    init(frontMatterTitle: String?) {
        accessibility.title = frontMatterTitle
    }

    /// Reads the statement at `lines[index]` if it is one, and returns the index of the next line.
    mutating func read(_ lines: [Document.Line], at index: Int) throws(MermaidError) -> Int? {
        var cursor = Cursor(lines[index])
        cursor.skipSpaces()
        if cursor.consume("accTitle") {
            cursor.skipSpaces()
            guard cursor.consume(":") else { throw cursor.error(":") }
            accessibility.title = Label.clean(cursor.rest())
            return index + 1
        }
        guard cursor.consume("accDescr") else { return nil }
        cursor.skipSpaces()
        if cursor.consume(":") {
            accessibility.description = Label.clean(cursor.rest())
            return index + 1
        }
        guard cursor.consume("{") else { throw cursor.error(": or {") }
        var parts: [String] = []
        let inline = cursor.rest()
        if let close = inline.firstIndex(of: "}") {
            accessibility.description = Label.clean(String(inline[..<close]))
            return index + 1
        }
        if !inline.trimmingCharacters(in: .whitespaces).isEmpty { parts.append(inline) }
        var next = index + 1
        while next < lines.count {
            let text = String(lines[next].characters)
            if let close = text.firstIndex(of: "}") {
                parts.append(String(text[..<close]))
                accessibility.description = parts
                    .map { $0.trimmingCharacters(in: .whitespaces) }
                    .filter { !$0.isEmpty }
                    .joined(separator: " ")
                return next + 1
            }
            parts.append(text)
            next += 1
        }
        throw cursor.error("}")
    }
}

/// Statements that style or link a diagram: accepted and not drawn.
enum Styling {
    static let keywords = ["classDef", "class", "style", "linkStyle", "click", "cssClass", "callback", "link"]

    static func matches(_ cursor: Cursor, keywords: [String] = keywords) -> Bool {
        keywords.contains { cursor.starts(withWord: $0) }
    }
}
