/// `flowchart`/`graph`: nodes with shapes, chains of links (`A-->B-->C`, `A & B --> C`), both
/// label forms (`-->|x|`, `-- x -->`), `subgraph`/`end`, `;` between statements.
struct FlowchartParser {
    private let document: Document
    private let header: Header
    private var accessibilityReader: AccessibilityReader
    private var direction = Direction.topToBottom
    private var nodes: [Flowchart.Node] = []
    private var nodeIndex: [String: Int] = [:]
    private var labelled: Set<String> = []
    private var edges: [Flowchart.Edge] = []
    private var subgraphs: [Flowchart.Subgraph] = []
    private var open: [(id: String, line: Int)] = []

    init(document: Document, header: Header) {
        self.document = document
        self.header = header
        accessibilityReader = AccessibilityReader(frontMatterTitle: document.frontMatterTitle)
    }

    mutating func parse() throws(MermaidError) -> Flowchart {
        var cursor = header.cursor
        cursor.skipSpaces()
        let directionStart = cursor.index
        let word = cursor.read { $0.isLetter }
        if !word.isEmpty {
            guard let parsed = Direction(keyword: word) else { throw cursor.error("direction", at: directionStart) }
            direction = parsed
        }
        try statements(&cursor)

        let lines = document.lines
        var index = header.lineIndex + 1
        while index < lines.count {
            if let next = try accessibilityReader.read(lines, at: index) {
                index = next
                continue
            }
            var line = Cursor(lines[index])
            try statements(&line)
            index += 1
        }
        if let unclosed = open.last {
            throw .syntax(line: unclosed.line, column: 1, expected: "end")
        }
        let subgraphIDs = Set(subgraphs.map(\.id))
        return Flowchart(
            direction: direction,
            nodes: nodes.filter { !subgraphIDs.contains($0.id) || labelled.contains($0.id) },
            edges: edges,
            subgraphs: subgraphs,
            accessibility: accessibilityReader.accessibility
        )
    }

    private mutating func statements(_ cursor: inout Cursor) throws(MermaidError) {
        while true {
            cursor.skipSpaces()
            if cursor.isAtEnd { return }
            if cursor.consume(";") { continue }
            try statement(&cursor)
        }
    }

    private mutating func statement(_ cursor: inout Cursor) throws(MermaidError) {
        if cursor.starts(withWord: "subgraph") {
            cursor.index += "subgraph".count
            try openSubgraph(&cursor)
            return
        }
        if cursor.starts(with: "end"), cursor.peek(3).map({ $0.isWhitespace || $0 == ";" }) ?? true {
            guard open.popLast() != nil else { throw cursor.error("statement") }
            cursor.index += 3
            return
        }
        if cursor.starts(withWord: "direction") {
            cursor.index += "direction".count
            cursor.skipSpaces()
            let start = cursor.index
            guard Direction(keyword: cursor.read { $0.isLetter }) != nil else { throw cursor.error("direction", at: start) }
            return
        }
        if Styling.matches(cursor, keywords: ["classDef", "class", "style", "linkStyle", "click"]) {
            _ = cursor.rest()
            return
        }
        try chain(&cursor)
    }

    private mutating func openSubgraph(_ cursor: inout Cursor) throws(MermaidError) {
        cursor.skipSpaces()
        var text = cursor.rest().trimmingCharacters(in: .whitespaces)
        if text.hasSuffix(";") { text.removeLast() }
        guard !text.isEmpty else { throw cursor.error("subgraph name") }
        let id: String
        let title: String
        if let bracket = text.firstIndex(of: "["), text.hasSuffix("]") {
            id = String(text[..<bracket]).trimmingCharacters(in: .whitespaces)
            title = Label.clean(String(text[text.index(after: bracket)..<text.index(before: text.endIndex)]))
        } else {
            title = Label.clean(text)
            id = title
        }
        guard !id.isEmpty else { throw .syntax(line: cursor.line, column: cursor.characters.count + 1, expected: "subgraph name") }
        subgraphs.append(Flowchart.Subgraph(id: id, title: title, parent: open.last?.id))
        open.append((id, cursor.line))
    }

    private mutating func chain(_ cursor: inout Cursor) throws(MermaidError) {
        var left = try group(&cursor)
        while true {
            cursor.skipSpaces()
            if cursor.isAtEnd || cursor.peek() == ";" { return }
            guard let link = try Link.read(&cursor) else { throw cursor.error("link") }
            cursor.skipSpaces()
            let right = try group(&cursor)
            for from in left {
                for to in right {
                    edges.append(Flowchart.Edge(
                        from: from, to: to, label: link.label, stroke: link.stroke, head: link.head, tail: link.tail
                    ))
                }
            }
            left = right
        }
    }

    private mutating func group(_ cursor: inout Cursor) throws(MermaidError) -> [String] {
        var ids = [try node(&cursor)]
        while true {
            let save = cursor.index
            cursor.skipSpaces()
            guard cursor.consume("&") else {
                cursor.index = save
                return ids
            }
            cursor.skipSpaces()
            ids.append(try node(&cursor))
        }
    }

    private static let shapes: [(open: String, close: String, shape: Flowchart.Shape)] = [
        ("([", "])", .stadium), ("[[", "]]", .subroutine), ("[(", ")]", .cylinder), ("((", "))", .circle),
        ("{{", "}}", .hexagon), ("[", "]", .rectangle), ("(", ")", .rounded), ("{", "}", .diamond),
    ]

    private mutating func node(_ cursor: inout Cursor) throws(MermaidError) -> String {
        guard let id = Identifier.read(&cursor) else { throw cursor.error("node") }
        var label: String?
        var shape = Flowchart.Shape.rectangle
        let save = cursor.index
        cursor.skipSpaces()
        if cursor.starts(with: "(((") || cursor.starts(with: "[/") || cursor.starts(with: "[\\")
            || cursor.starts(with: ">") || cursor.starts(with: "@{") {
            throw cursor.error("node shape")
        }
        if let match = Self.shapes.first(where: { cursor.starts(with: $0.open) }) {
            let openAt = cursor.index
            cursor.index += match.open.count
            label = try Self.shapeText(&cursor, close: match.close, openedAt: openAt)
            shape = match.shape
        } else {
            cursor.index = save
        }
        if cursor.consume(":::") {
            _ = cursor.read { Identifier.isBody($0) || $0 == "-" }
        }
        register(id, label: label, shape: shape)
        return id
    }

    private static func shapeText(_ cursor: inout Cursor, close: String, openedAt: Int) throws(MermaidError) -> String {
        if cursor.peek() == "\"" {
            cursor.index += 1
            guard let quoted = cursor.read(until: "\"") else { throw cursor.error("\"", at: cursor.characters.count) }
            guard cursor.consume(close) else { throw cursor.error(close) }
            return Label.clean(quoted)
        }
        guard let text = cursor.read(until: close) else { throw cursor.error(close, at: cursor.characters.count) }
        return Label.clean(text)
    }

    private mutating func register(_ id: String, label: String?, shape: Flowchart.Shape) {
        if let index = nodeIndex[id] {
            if let label {
                nodes[index].label = label
                nodes[index].shape = shape
                labelled.insert(id)
            }
            return
        }
        if label != nil { labelled.insert(id) }
        nodeIndex[id] = nodes.count
        nodes.append(Flowchart.Node(id: id, label: label ?? id, shape: shape, subgraph: open.last?.id))
    }
}

/// One link between node groups: `-->`, `---`, `-.->`, `==>`, `--x`, `--o`, `<-->`, with its label.
struct Link {
    var stroke: Flowchart.Stroke
    var head: Flowchart.Head
    var tail: Flowchart.Head
    var label: String?

    /// Nil, with nothing consumed, when no link starts at the cursor.
    static func read(_ cursor: inout Cursor) throws(MermaidError) -> Link? {
        let start = cursor.index
        var tail = Flowchart.Head.none
        if cursor.peek() == "<" {
            tail = .arrow
            cursor.index += 1
        } else if let first = cursor.peek(), first == "x" || first == "o",
                  let next = cursor.peek(1), next == "-" || next == "=" {
            tail = first == "x" ? .cross : .circle
            cursor.index += 1
        }
        guard var link = try body(&cursor) else {
            cursor.index = start
            return nil
        }
        link.tail = tail
        let save = cursor.index
        cursor.skipSpaces()
        if cursor.consume("|") {
            guard let text = cursor.read(until: "|") else { throw cursor.error("|", at: cursor.characters.count) }
            link.label = Label.clean(text)
        } else {
            cursor.index = save
        }
        return link
    }

    private static func head(_ cursor: inout Cursor) -> Flowchart.Head? {
        guard let character = cursor.peek() else { return nil }
        if character == ">" {
            cursor.index += 1
            return .arrow
        }
        if character == "x" || character == "o", cursor.peek(1).map({ !Identifier.isBody($0) }) ?? true {
            cursor.index += 1
            return character == "x" ? .cross : .circle
        }
        return nil
    }

    private static func body(_ cursor: inout Cursor) throws(MermaidError) -> Link? {
        switch cursor.peek() {
        case "-" where cursor.peek(1) == ".":
            cursor.index += 1
            let dots = cursor.read { $0 == "." }
            if cursor.consume("-") {
                return Link(stroke: .dotted, head: head(&cursor) ?? .none, tail: .none)
            }
            guard dots.count == 1, cursor.peek()?.isWhitespace == true else { return nil }
            return try labelled(&cursor, stroke: .dotted)
        case "-":
            let dashes = cursor.read { $0 == "-" }.count
            guard dashes >= 2 else { return nil }
            if let head = head(&cursor) {
                return Link(stroke: .solid, head: head, tail: .none)
            }
            if dashes >= 3 {
                return Link(stroke: .solid, head: .none, tail: .none)
            }
            return try labelled(&cursor, stroke: .solid)
        case "=":
            let bars = cursor.read { $0 == "=" }.count
            guard bars >= 2 else { return nil }
            if let head = head(&cursor) {
                return Link(stroke: .thick, head: head, tail: .none)
            }
            if bars >= 3 {
                return Link(stroke: .thick, head: .none, tail: .none)
            }
            return try labelled(&cursor, stroke: .thick)
        default:
            return nil
        }
    }

    /// `-- text -->`, `-. text .->`, `== text ==>`: the text runs to the first closing link.
    private static func labelled(_ cursor: inout Cursor, stroke: Flowchart.Stroke) throws(MermaidError) -> Link {
        let textStart = cursor.index
        var position = cursor.index
        let characters = cursor.characters
        func at(_ offset: Int) -> Character? {
            position + offset < characters.count ? characters[position + offset] : nil
        }
        while position < characters.count {
            switch stroke {
            case .solid, .thick:
                let mark: Character = stroke == .solid ? "-" : "="
                if characters[position] == mark, at(1) == mark {
                    var run = 0
                    while at(run) == mark { run += 1 }
                    var probe = Cursor(Document.Line(number: cursor.line, characters: characters), from: position + run)
                    if let head = head(&probe) {
                        return finish(&cursor, text: textStart..<position, end: probe.index, stroke: stroke, head: head)
                    }
                    if run >= 3 {
                        return finish(&cursor, text: textStart..<position, end: position + run, stroke: stroke, head: .none)
                    }
                    position += run
                    continue
                }
            case .dotted:
                if characters[position] == ".", at(1) == "-" {
                    var probe = Cursor(Document.Line(number: cursor.line, characters: characters), from: position + 2)
                    let head = head(&probe) ?? .none
                    return finish(&cursor, text: textStart..<position, end: probe.index, stroke: .dotted, head: head)
                }
            }
            position += 1
        }
        throw cursor.error("link end", at: characters.count)
    }

    private static func finish(
        _ cursor: inout Cursor, text: Range<Int>, end: Int, stroke: Flowchart.Stroke, head: Flowchart.Head
    ) -> Link {
        let label = Label.clean(String(cursor.characters[text]))
        cursor.index = end
        return Link(stroke: stroke, head: head, tail: .none, label: label.isEmpty ? nil : label)
    }
}
