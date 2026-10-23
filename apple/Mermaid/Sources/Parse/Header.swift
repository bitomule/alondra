/// The first statement of the block: which diagram it is, and what follows the keyword on its line.
struct Header {
    enum Kind {
        case flowchart, sequence, state, classDiagram
    }

    let kind: Kind
    /// Index in `Document.lines` of the header line.
    let lineIndex: Int
    /// Where the keyword ends on that line.
    let cursor: Cursor

    static func read(_ document: Document) throws(MermaidError) -> Header {
        guard let index = document.lines.firstIndex(where: { !$0.isBlank }) else {
            throw .syntax(line: 1, column: 1, expected: "diagram type")
        }
        var cursor = Cursor(document.lines[index])
        cursor.skipSpaces()
        let start = cursor.index
        let keyword = cursor.read { !$0.isWhitespace && $0 != ";" }
        let kind: Kind
        switch keyword {
        case "flowchart", "graph": kind = .flowchart
        case "sequenceDiagram": kind = .sequence
        case "stateDiagram", "stateDiagram-v2": kind = .state
        case "classDiagram", "classDiagram-v2": kind = .classDiagram
        case "":
            throw cursor.error("diagram type", at: start)
        default:
            throw .unsupported(kind: keyword)
        }
        return Header(kind: kind, lineIndex: index, cursor: cursor)
    }
}
