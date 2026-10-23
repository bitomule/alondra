/// `stateDiagram`/`stateDiagram-v2`: states with descriptions, `[*]`, labelled transitions,
/// composite states, `<<choice>>`/`<<fork>>`/`<<join>>` and notes.
struct StateParser {
    private let document: Document
    private let header: Header
    private var accessibilityReader: AccessibilityReader
    private var direction = Direction.topToBottom
    private var states: [StateDiagram.State] = []
    private var stateIndex: [String: Int] = [:]
    private var described: Set<String> = []
    private var transitions: [StateDiagram.Transition] = []
    private var composites: [StateDiagram.Composite] = []
    private var notes: [StateDiagram.Note] = []
    private var open: [(id: String, line: Int)] = []

    init(document: Document, header: Header) {
        self.document = document
        self.header = header
        accessibilityReader = AccessibilityReader(frontMatterTitle: document.frontMatterTitle)
    }

    mutating func parse() throws(MermaidError) -> StateDiagram {
        var headerRest = header.cursor
        try headerRest.expectEnd()
        let lines = document.lines
        var index = header.lineIndex + 1
        while index < lines.count {
            if let next = try accessibilityReader.read(lines, at: index) {
                index = next
                continue
            }
            var cursor = Cursor(lines[index])
            cursor.skipSpaces()
            if cursor.starts(withWord: "note") {
                index = try note(&cursor, lines: lines, index: index)
                continue
            }
            if !cursor.isAtEnd {
                try statement(&cursor)
            }
            index += 1
        }
        if let unclosed = open.last {
            throw .syntax(line: unclosed.line, column: 1, expected: "}")
        }
        return StateDiagram(
            direction: direction, states: states, transitions: transitions, composites: composites, notes: notes,
            accessibility: accessibilityReader.accessibility
        )
    }

    private mutating func statement(_ cursor: inout Cursor) throws(MermaidError) {
        if cursor.consume("}") {
            guard open.popLast() != nil else { throw cursor.error("statement", at: cursor.index - 1) }
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
            if open.isEmpty { direction = parsed }
            try cursor.expectEnd()
            return
        }
        if Styling.matches(cursor, keywords: ["classDef", "class", "style"]) {
            _ = cursor.rest()
            return
        }
        if cursor.starts(withWord: "state") {
            cursor.index += "state".count
            try declaration(&cursor)
            return
        }
        let first = try endpoint(&cursor, asSource: true)
        cursor.skipSpaces()
        if cursor.consume("-->") {
            cursor.skipSpaces()
            let second = try endpoint(&cursor, asSource: false)
            cursor.skipSpaces()
            var label: String?
            if cursor.consume(":") {
                let text = Label.clean(cursor.rest())
                label = text.isEmpty ? nil : text
            }
            try cursor.expectEnd()
            transitions.append(.init(from: first, to: second, label: label))
            return
        }
        if cursor.consume(":") {
            describe(first, Label.clean(cursor.rest()))
            return
        }
        try cursor.expectEnd()
    }

    /// `state "Description" as Id`, `state Id <<choice>>`, `state Id {`, `state Id : description`.
    private mutating func declaration(_ cursor: inout Cursor) throws(MermaidError) {
        cursor.skipSpaces()
        var label: String?
        if cursor.peek() == "\"" {
            cursor.index += 1
            guard let quoted = cursor.read(until: "\"") else { throw cursor.error("\"", at: cursor.characters.count) }
            label = Label.clean(quoted)
            cursor.skipSpaces()
            guard cursor.consume("as") else { throw cursor.error("as") }
            cursor.skipSpaces()
        }
        guard let id = Identifier.read(&cursor) else { throw cursor.error("state") }
        cursor.skipSpaces()
        if cursor.consume("<<") {
            let start = cursor.index
            guard let word = cursor.read(until: ">>") else { throw cursor.error(">>", at: cursor.characters.count) }
            let kind: StateDiagram.Kind
            switch word {
            case "choice": kind = .choice
            case "fork": kind = .fork
            case "join": kind = .join
            default: throw cursor.error("choice, fork or join", at: start)
            }
            try cursor.expectEnd()
            register(id, kind: kind)
            if let index = stateIndex[id] {
                states[index].kind = kind
                states[index].label = ""
            }
            return
        }
        if cursor.consume("{") {
            try cursor.expectEnd()
            openComposite(id, label: label, line: cursor.line)
            return
        }
        if cursor.consume(":") {
            register(id, kind: .normal)
            describe(id, Label.clean(cursor.rest()))
            return
        }
        try cursor.expectEnd()
        register(id, kind: .normal)
        if let label { describe(id, label) }
    }

    private mutating func openComposite(_ id: String, label: String?, line: Int) {
        var parent = open.last?.id
        if let index = stateIndex[id] {
            parent = states[index].parent
            states.remove(at: index)
            stateIndex = Dictionary(uniqueKeysWithValues: states.enumerated().map { ($1.id, $0) })
        }
        if let existing = composites.firstIndex(where: { $0.id == id }) {
            if let label { composites[existing].label = label }
        } else {
            composites.append(.init(id: id, label: label ?? id, parent: parent))
        }
        open.append((id, line))
    }

    private mutating func endpoint(_ cursor: inout Cursor, asSource: Bool) throws(MermaidError) -> String {
        if cursor.consume("[*]") {
            let scope = open.last.map { "@\($0.id)" } ?? ""
            let id = (asSource ? "[*]start" : "[*]end") + scope
            register(id, kind: asSource ? .start : .end)
            return id
        }
        guard let id = Identifier.read(&cursor) else { throw cursor.error("state") }
        if cursor.consume(":::") {
            _ = cursor.read { Identifier.isBody($0) || $0 == "-" }
        }
        register(id, kind: .normal)
        return id
    }

    private mutating func register(_ id: String, kind: StateDiagram.Kind) {
        guard stateIndex[id] == nil, !composites.contains(where: { $0.id == id }) else { return }
        stateIndex[id] = states.count
        states.append(.init(id: id, label: kind == .normal ? id : "", kind: kind, parent: open.last?.id))
    }

    private mutating func describe(_ id: String, _ text: String) {
        guard !text.isEmpty else { return }
        if let composite = composites.firstIndex(where: { $0.id == id }) {
            composites[composite].label = text
            return
        }
        guard let index = stateIndex[id] else { return }
        states[index].label = described.contains(id) ? states[index].label + "\n" + text : text
        described.insert(id)
    }

    /// `note left of Id : text`, or the lines up to `end note`.
    private mutating func note(_ cursor: inout Cursor, lines: [Document.Line], index: Int) throws(MermaidError) -> Int {
        cursor.index += "note".count
        cursor.skipSpaces()
        let side: StateDiagram.Note.Side
        if cursor.consume("left of") {
            side = .left
        } else if cursor.consume("right of") {
            side = .right
        } else {
            throw cursor.error("left of or right of")
        }
        cursor.skipSpaces()
        guard let id = Identifier.read(&cursor) else { throw cursor.error("state") }
        register(id, kind: .normal)
        cursor.skipSpaces()
        if cursor.consume(":") {
            notes.append(.init(state: id, side: side, text: Label.clean(cursor.rest())))
            return index + 1
        }
        try cursor.expectEnd()
        var text: [String] = []
        var next = index + 1
        while next < lines.count {
            let line = String(lines[next].characters).trimmingCharacters(in: .whitespaces)
            if line == "end note" {
                notes.append(.init(state: id, side: side, text: Label.clean(text.joined(separator: "\n"))))
                return next + 1
            }
            text.append(line)
            next += 1
        }
        throw .syntax(line: cursor.line, column: 1, expected: "end note")
    }
}
