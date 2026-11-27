import Foundation

/// The steps of a scenario as rows a person edits, and the text Gherkin reads them from, which is
/// what the daemon sends and takes: one step per line, tables between `|`, a text block between
/// `"""` and an `Examples:` table at the end.
public struct StepsDraft: Equatable, Sendable {
    public enum Keyword: String, CaseIterable, Sendable {
        case given = "Given", when = "When", then = "Then", and = "And", but = "But"
    }

    public struct Row: Equatable, Identifiable, Sendable {
        public let id: Int
        public var keyword: Keyword
        public var text: String
        public var table: [[String]]
        public var doc: String?

        public init(id: Int, keyword: Keyword, text: String = "", table: [[String]] = [], doc: String? = nil) {
            self.id = id; self.keyword = keyword; self.text = text; self.table = table; self.doc = doc
        }
    }

    public var rows: [Row]
    public var examples: [[String]]
    private var nextID: Int

    public init(rows: [Row] = [], examples: [[String]] = []) {
        self.rows = rows
        self.examples = examples
        nextID = (rows.map(\.id).max() ?? -1) + 1
    }

    /// Reads the text; nil when a line is not a step, a table row or a text block, so the person
    /// edits the text itself rather than lose a line.
    public init?(parsing text: String) {
        var rows: [Row] = []
        var examples: [[String]] = []
        var inExamples = false
        var doc: [String]?
        for line in text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if doc != nil {
                if trimmed == "\"\"\"" {
                    rows[rows.count - 1].doc = doc?.joined(separator: "\n")
                    doc = nil
                } else {
                    doc?.append(line.hasPrefix("  ") ? String(line.dropFirst(2)) : line)
                }
                continue
            }
            if trimmed.isEmpty { continue }
            if trimmed == "Examples:" { inExamples = true; continue }
            if trimmed == "\"\"\"" {
                guard !rows.isEmpty, !inExamples else { return nil }
                doc = []
                continue
            }
            if trimmed.hasPrefix("|") {
                let cells = Self.cells(of: trimmed)
                if inExamples { examples.append(cells) } else if rows.isEmpty { return nil } else { rows[rows.count - 1].table.append(cells) }
                continue
            }
            guard !inExamples, let (keyword, rest) = Self.keyword(of: trimmed) else { return nil }
            rows.append(Row(id: rows.count, keyword: keyword, text: rest))
        }
        guard doc == nil else { return nil }
        self.init(rows: rows, examples: examples)
    }

    public var text: String {
        var lines: [String] = []
        for row in rows {
            lines.append(row.text.isEmpty ? row.keyword.rawValue : "\(row.keyword.rawValue) \(row.text)")
            lines.append(contentsOf: row.table.map(Self.line))
            if let doc = row.doc {
                lines.append("  \"\"\"")
                lines.append(contentsOf: doc.split(separator: "\n", omittingEmptySubsequences: false).map { $0.isEmpty ? "" : "  \($0)" })
                lines.append("  \"\"\"")
            }
        }
        if !examples.isEmpty {
            lines.append("")
            lines.append("Examples:")
            lines.append(contentsOf: examples.map(Self.line))
        }
        return lines.joined(separator: "\n")
    }

    public mutating func add(_ keyword: Keyword) {
        rows.append(Row(id: nextID, keyword: keyword))
        nextID += 1
    }

    public mutating func remove(_ id: Int) { rows.removeAll { $0.id == id } }

    public mutating func move(_ id: Int, by offset: Int) {
        guard let index = rows.firstIndex(where: { $0.id == id }) else { return }
        let target = index + offset
        guard rows.indices.contains(target) else { return }
        rows.swapAt(index, target)
    }

    public func canMove(_ id: Int, by offset: Int) -> Bool {
        guard let index = rows.firstIndex(where: { $0.id == id }) else { return false }
        return rows.indices.contains(index + offset)
    }

    private static func keyword(of line: String) -> (Keyword, String)? {
        for keyword in Keyword.allCases {
            if line == keyword.rawValue { return (keyword, "") }
            if line.hasPrefix(keyword.rawValue + " ") { return (keyword, String(line.dropFirst(keyword.rawValue.count + 1))) }
        }
        return nil
    }

    static func cells(of line: String) -> [String] {
        var cells: [String] = []
        var current = ""
        var escaped = false
        var started = false
        for character in line {
            if escaped {
                switch character {
                case "n": current.append("\n")
                case "|", "\\": current.append(character)
                default: current.append("\\"); current.append(character)
                }
                escaped = false
            } else if character == "\\" {
                escaped = true
            } else if character == "|" {
                if started { cells.append(current.trimmingCharacters(in: .whitespaces)) }
                current = ""
                started = true
            } else {
                current.append(character)
            }
        }
        return cells
    }

    private static func line(_ cells: [String]) -> String {
        let escaped = cells.map { $0.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "|", with: "\\|").replacingOccurrences(of: "\n", with: "\\n") }
        return "  | " + escaped.joined(separator: " | ") + " |"
    }
}
