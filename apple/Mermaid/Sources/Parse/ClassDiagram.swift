/// `classDiagram`: `class X { … }` blocks, loose members (`X : +field`), relations with their
/// cardinalities and label, `<<annotation>>`, generics (`~T~`) and namespaces.
struct ClassParser {
    private let document: Document
    private let header: Header
    private var accessibilityReader: AccessibilityReader
    private var direction = Direction.topToBottom
    private var classes: [ClassDiagram.Class] = []
    private var classIndex: [String: Int] = [:]
    private var relations: [ClassDiagram.Relation] = []
    private var namespaces: [String] = []
    private var namespace: (id: String, line: Int)?
    private var body: (id: String, line: Int)?

    init(document: Document, header: Header) {
        self.document = document
        self.header = header
        accessibilityReader = AccessibilityReader(frontMatterTitle: document.frontMatterTitle)
    }

    mutating func parse() throws(MermaidError) -> ClassDiagram {
        var headerRest = header.cursor
        try headerRest.expectEnd()
        let lines = document.lines
        var index = header.lineIndex + 1
        while index < lines.count {
            if body == nil, let next = try accessibilityReader.read(lines, at: index) {
                index = next
                continue
            }
            var cursor = Cursor(lines[index])
            cursor.skipSpaces()
            if !cursor.isAtEnd {
                if let open = body {
                    try member(&cursor, of: open.id)
                } else {
                    try statement(&cursor)
                }
            }
            index += 1
        }
        if let unclosed = body ?? namespace {
            throw .syntax(line: unclosed.line, column: 1, expected: "}")
        }
        return ClassDiagram(
            direction: direction, classes: classes, relations: relations, namespaces: namespaces,
            accessibility: accessibilityReader.accessibility
        )
    }

    private mutating func statement(_ cursor: inout Cursor) throws(MermaidError) {
        if cursor.consume("}") {
            guard namespace != nil else { throw cursor.error("statement", at: cursor.index - 1) }
            namespace = nil
            try cursor.expectEnd()
            return
        }
        if cursor.starts(withWord: "direction") {
            cursor.index += "direction".count
            cursor.skipSpaces()
            let start = cursor.index
            guard let parsed = Direction(keyword: cursor.read { $0.isLetter }) else {
                throw cursor.error("direction", at: start)
            }
            direction = parsed
            try cursor.expectEnd()
            return
        }
        if cursor.starts(withWord: "namespace") {
            guard namespace == nil else { throw cursor.error("statement") }
            cursor.index += "namespace".count
            cursor.skipSpaces()
            guard let id = Identifier.read(&cursor) else { throw cursor.error("namespace name") }
            cursor.skipSpaces()
            guard cursor.consume("{") else { throw cursor.error("{") }
            try cursor.expectEnd()
            if !namespaces.contains(id) { namespaces.append(id) }
            namespace = (id, cursor.line)
            return
        }
        if Styling.matches(cursor, keywords: ["classDef", "cssClass", "style", "click", "callback", "link"]) {
            _ = cursor.rest()
            return
        }
        if cursor.starts(withWord: "class") {
            cursor.index += "class".count
            try declaration(&cursor)
            return
        }
        if cursor.consume("<<") {
            let start = cursor.index
            guard let annotation = cursor.read(until: ">>") else { throw cursor.error(">>", at: cursor.characters.count) }
            guard !annotation.isEmpty else { throw cursor.error("annotation", at: start) }
            cursor.skipSpaces()
            let id = try name(&cursor)
            try cursor.expectEnd()
            classes[classIndex[id]!].annotation = annotation
            return
        }
        let first = try name(&cursor)
        cursor.skipSpaces()
        if cursor.consume(":") {
            add(Label.generics(cursor.rest().trimmingCharacters(in: .whitespaces)), to: first)
            return
        }
        if cursor.isAtEnd { return }
        try relation(&cursor, from: first)
    }

    /// `class Name`, `class Name~T~`, `class Name["Label"]`, `class Name {`.
    private mutating func declaration(_ cursor: inout Cursor) throws(MermaidError) {
        cursor.skipSpaces()
        let id = try name(&cursor)
        if cursor.consume("[") {
            guard let text = cursor.read(until: "]") else { throw cursor.error("]", at: cursor.characters.count) }
            classes[classIndex[id]!].label = Label.clean(text)
        }
        if cursor.consume(":::") {
            _ = cursor.read { Identifier.isBody($0) || $0 == "-" }
        }
        cursor.skipSpaces()
        if cursor.consume("{") {
            cursor.skipSpaces()
            if cursor.consume("}") {
                try cursor.expectEnd()
                return
            }
            try cursor.expectEnd()
            body = (id, cursor.line)
            return
        }
        try cursor.expectEnd()
    }

    /// A class name, plain or in backticks, with its generic parameter; declared on first mention.
    private mutating func name(_ cursor: inout Cursor) throws(MermaidError) -> String {
        var id: String
        if cursor.consume("`") {
            guard let quoted = cursor.read(until: "`"), !quoted.isEmpty else { throw cursor.error("`", at: cursor.characters.count) }
            id = quoted
        } else {
            guard let word = Identifier.read(&cursor) else { throw cursor.error("class") }
            id = word
        }
        var label = id
        if cursor.consume("~") {
            guard let generic = cursor.read(until: "~") else { throw cursor.error("~", at: cursor.characters.count) }
            label = "\(id)<\(Label.generics(generic))>"
        }
        if let index = classIndex[id] {
            if label != id { classes[index].label = label }
        } else {
            classIndex[id] = classes.count
            classes.append(.init(
                id: id, label: label, annotation: nil, attributes: [], methods: [], namespace: namespace?.id
            ))
        }
        return id
    }

    private mutating func member(_ cursor: inout Cursor, of id: String) throws(MermaidError) {
        if cursor.consume("}") {
            try cursor.expectEnd()
            body = nil
            return
        }
        if cursor.consume("<<") {
            guard let annotation = cursor.read(until: ">>") else { throw cursor.error(">>", at: cursor.characters.count) }
            try cursor.expectEnd()
            classes[classIndex[id]!].annotation = annotation
            return
        }
        add(Label.generics(cursor.rest().trimmingCharacters(in: .whitespaces)), to: id)
    }

    private mutating func add(_ member: String, to id: String) {
        guard !member.isEmpty, let index = classIndex[id] else { return }
        if member.contains("(") {
            classes[index].methods.append(member)
        } else {
            classes[index].attributes.append(member)
        }
    }

    private static let leftEnds: [(String, ClassDiagram.End)] = [
        ("<|", .inheritance), ("*", .composition), ("o", .aggregation), ("<", .arrow),
    ]

    private static let rightEnds: [(String, ClassDiagram.End)] = [
        ("|>", .inheritance), ("*", .composition), ("o", .aggregation), (">", .arrow),
    ]

    /// `A "1" *-- "many" B : label`.
    private mutating func relation(_ cursor: inout Cursor, from: String) throws(MermaidError) {
        let fromCardinality = try cardinality(&cursor)
        let operatorStart = cursor.index
        var fromEnd = ClassDiagram.End.none
        for (text, end) in Self.leftEnds where cursor.starts(with: text) {
            let after = text.count
            if cursor.peek(after) == "-" || cursor.peek(after) == "." {
                fromEnd = end
                cursor.index += after
                break
            }
        }
        let stroke: ClassDiagram.Stroke
        if cursor.consume("--") {
            stroke = .solid
        } else if cursor.consume("..") {
            stroke = .dashed
        } else {
            throw cursor.error("relation", at: operatorStart)
        }
        var toEnd = ClassDiagram.End.none
        for (text, end) in Self.rightEnds where cursor.starts(with: text) {
            let follower = cursor.peek(text.count)
            if text == "o", let follower, !follower.isWhitespace, follower != "\"" { continue }
            toEnd = end
            cursor.index += text.count
            break
        }
        cursor.skipSpaces()
        let toCardinality = try cardinality(&cursor)
        let to = try name(&cursor)
        cursor.skipSpaces()
        var label: String?
        if cursor.consume(":") {
            let text = Label.clean(cursor.rest())
            label = text.isEmpty ? nil : text
        }
        try cursor.expectEnd()
        relations.append(.init(
            from: from, to: to, fromEnd: fromEnd, toEnd: toEnd, stroke: stroke,
            fromCardinality: fromCardinality, toCardinality: toCardinality, label: label
        ))
    }

    private func cardinality(_ cursor: inout Cursor) throws(MermaidError) -> String? {
        guard cursor.consume("\"") else { return nil }
        guard let text = cursor.read(until: "\"") else { throw cursor.error("\"", at: cursor.characters.count) }
        cursor.skipSpaces()
        return text
    }
}
