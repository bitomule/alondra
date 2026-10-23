/// `sequenceDiagram`: participants and actors with aliases, messages, notes, `loop`/`alt`/`opt`/
/// `par`/`critical`/`break` blocks, activations and `autonumber`.
struct SequenceParser {
    private struct OpenBlock {
        var block: SequenceDiagram.Block
        let line: Int
    }

    private let document: Document
    private let header: Header
    private var accessibilityReader: AccessibilityReader
    private var participants: [SequenceDiagram.Participant] = []
    private var participantIndex: [String: Int] = [:]
    private var root: [SequenceDiagram.Item] = []
    private var open: [OpenBlock] = []
    private var autonumber = false

    init(document: Document, header: Header) {
        self.document = document
        self.header = header
        accessibilityReader = AccessibilityReader(frontMatterTitle: document.frontMatterTitle)
    }

    mutating func parse() throws(MermaidError) -> SequenceDiagram {
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
            if !cursor.isAtEnd {
                try statement(&cursor)
            }
            index += 1
        }
        if let unclosed = open.last {
            throw .syntax(line: unclosed.line, column: 1, expected: "end")
        }
        return SequenceDiagram(
            participants: participants, items: root, autonumber: autonumber, accessibility: accessibilityReader.accessibility
        )
    }

    private static let blockKinds: [(String, SequenceDiagram.Block.Kind)] = [
        ("loop", .loop), ("alt", .alt), ("opt", .opt), ("par", .par), ("critical", .critical), ("break", .break),
    ]

    private static let dividers: [(String, SequenceDiagram.Block.Kind)] = [
        ("else", .alt), ("and", .par), ("option", .critical),
    ]

    private mutating func statement(_ cursor: inout Cursor) throws(MermaidError) {
        for keyword in ["participant", "actor"] where cursor.starts(withWord: keyword) {
            cursor.index += keyword.count
            try declare(&cursor, kind: keyword == "actor" ? .actor : .participant)
            return
        }
        if cursor.starts(withWord: "autonumber") {
            autonumber = true
            _ = cursor.rest()
            return
        }
        if cursor.starts(withWord: "title") {
            cursor.index += "title".count
            let title = Label.clean(cursor.rest())
            if accessibilityReader.accessibility.title == nil, !title.isEmpty {
                accessibilityReader.accessibility.title = title
            }
            return
        }
        for keyword in ["activate", "deactivate"] where cursor.starts(withWord: keyword) {
            cursor.index += keyword.count
            cursor.skipSpaces()
            let id = participantID(cursor.rest())
            guard !id.isEmpty else { throw cursor.error("participant") }
            mention(id)
            append(keyword == "activate" ? .activate(id) : .deactivate(id))
            return
        }
        if cursor.starts(withWord: "note", caseInsensitive: true) {
            cursor.index += 4
            try note(&cursor)
            return
        }
        for (keyword, kind) in Self.blockKinds where cursor.starts(withWord: keyword) {
            cursor.index += keyword.count
            open.append(OpenBlock(
                block: .init(kind: kind, sections: [.init(label: Label.clean(cursor.rest()), items: [])]), line: cursor.line
            ))
            return
        }
        for (keyword, kind) in Self.dividers where cursor.starts(withWord: keyword) {
            guard let current = open.last, current.block.kind == kind else { throw cursor.error("statement") }
            cursor.index += keyword.count
            open[open.count - 1].block.sections.append(.init(label: Label.clean(cursor.rest()), items: []))
            return
        }
        if cursor.starts(withWord: "end") {
            cursor.index += 3
            try cursor.expectEnd()
            guard let closed = open.popLast() else { throw cursor.error("statement", at: cursor.index - 3) }
            append(.block(closed.block))
            return
        }
        try message(&cursor)
    }

    private mutating func declare(_ cursor: inout Cursor, kind: SequenceDiagram.Participant.Kind) throws(MermaidError) {
        cursor.skipSpaces()
        let start = cursor.index
        let text = cursor.rest()
        let id: String
        let label: String
        if let alias = text.range(of: " as ") {
            id = participantID(String(text[..<alias.lowerBound]))
            label = Label.clean(String(text[alias.upperBound...]))
        } else {
            id = participantID(text)
            label = id
        }
        guard !id.isEmpty, !id.contains("@") else { throw cursor.error("participant", at: start) }
        if let index = participantIndex[id] {
            participants[index].label = label
            participants[index].kind = kind
        } else {
            participantIndex[id] = participants.count
            participants.append(.init(id: id, label: label, kind: kind))
        }
    }

    private func participantID(_ raw: String) -> String {
        raw.trimmingCharacters(in: .whitespaces)
    }

    private mutating func mention(_ id: String) {
        guard participantIndex[id] == nil else { return }
        participantIndex[id] = participants.count
        participants.append(.init(id: id, label: id, kind: .participant))
    }

    private mutating func note(_ cursor: inout Cursor) throws(MermaidError) {
        cursor.skipSpaces()
        let placement: SequenceDiagram.Note.Placement
        if cursor.consume("left of") {
            placement = .leftOf
        } else if cursor.consume("right of") {
            placement = .rightOf
        } else if cursor.consume("over") {
            placement = .over
        } else {
            throw cursor.error("left of, right of or over")
        }
        cursor.skipSpaces()
        let start = cursor.index
        guard let targets = cursor.read(until: ":") else { throw cursor.error(":", at: cursor.characters.count) }
        let ids = targets.split(separator: ",").map { participantID(String($0)) }
        guard !ids.isEmpty, ids.allSatisfy({ !$0.isEmpty }), placement == .over || ids.count == 1 else {
            throw cursor.error("participant", at: start)
        }
        ids.forEach { mention($0) }
        append(.note(.init(placement: placement, participants: ids, text: Label.clean(cursor.rest()))))
    }

    private static let arrows: [(String, SequenceDiagram.Stroke, SequenceDiagram.Head)] = [
        ("-->>", .dotted, .arrow), ("--x", .dotted, .cross), ("--)", .dotted, .open), ("-->", .dotted, .none),
        ("->>", .solid, .arrow), ("-x", .solid, .cross), ("-)", .solid, .open), ("->", .solid, .none),
    ]

    private mutating func message(_ cursor: inout Cursor) throws(MermaidError) {
        let start = cursor.index
        while !cursor.isAtEnd, !Self.arrows.contains(where: { cursor.starts(with: $0.0) }) {
            if cursor.peek() == ":" || cursor.peek() == "," || cursor.peek() == "<" { break }
            cursor.index += 1
        }
        let from = participantID(String(cursor.characters[start..<cursor.index]))
        guard !from.isEmpty else { throw cursor.error("statement", at: start) }
        guard let arrow = Self.arrows.first(where: { cursor.starts(with: $0.0) }) else { throw cursor.error("arrow") }
        cursor.index += arrow.0.count
        var activation = SequenceDiagram.Activation.none
        if cursor.consume("+") {
            activation = .activateTarget
        } else if cursor.consume("-") {
            activation = .deactivateSource
        }
        cursor.skipSpaces()
        let targetStart = cursor.index
        let target = cursor.read { $0 != ":" }
        let to = participantID(target)
        guard !to.isEmpty, !to.hasPrefix("-"), !to.hasPrefix(">") else {
            throw cursor.error("participant", at: targetStart)
        }
        let text = cursor.consume(":") ? Label.clean(cursor.rest()) : ""
        mention(from)
        mention(to)
        append(.message(.init(
            from: from, to: to, text: text, stroke: arrow.1, head: arrow.2, activation: activation
        )))
    }

    private mutating func append(_ item: SequenceDiagram.Item) {
        if open.isEmpty {
            root.append(item)
        } else {
            let last = open[open.count - 1].block.sections.count - 1
            open[open.count - 1].block.sections[last].items.append(item)
        }
    }
}
