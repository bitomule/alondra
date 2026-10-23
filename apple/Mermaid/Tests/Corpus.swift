@testable import AlondraMermaid
import Foundation

/// The `.mmd` files under `Fixtures/`, with what each one must produce.
enum Corpus {
    static var directory: URL {
        let environment = ProcessInfo.processInfo.environment
        if let root = environment["TEST_SRCDIR"] {
            return URL(fileURLWithPath: root)
                .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("apple/Mermaid/Fixtures")
        }
        return URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../Fixtures").standardizedFileURL
    }

    static func names(_ folder: String) -> [String] {
        let path = directory.appendingPathComponent(folder).path
        let files = (try? FileManager.default.contentsOfDirectory(atPath: path)) ?? []
        return files.filter { $0.hasSuffix(".mmd") }.map { String($0.dropLast(4)) }.sorted()
    }

    static var valid: [String] { names("valid") }
    static var invalid: [String] { names("invalid") }

    static func read(_ folder: String, _ name: String, _ suffix: String) -> String? {
        try? String(contentsOf: directory.appendingPathComponent("\(folder)/\(name).\(suffix)"), encoding: .utf8)
    }

    static func source(_ name: String) -> String { read("valid", name, "mmd") ?? "" }

    static func diagram(_ name: String) throws -> Diagram { try Mermaid.parse(source(name)) }

    static let typography = MermaidTypography(size: 14)

    /// With `MERMAID_FIXTURES_RECORD=1`, writes `actual` next to Bazel's test outputs so the
    /// expected file can be reviewed and copied into `Fixtures/`.
    static func record(_ actual: String, as file: String) {
        let environment = ProcessInfo.processInfo.environment
        guard environment["MERMAID_FIXTURES_RECORD"] == "1", let outputs = environment["TEST_UNDECLARED_OUTPUTS_DIR"] else { return }
        try? actual.write(toFile: "\(outputs)/\(file)", atomically: true, encoding: .utf8)
    }
}

/// A line per element of the parsed model, so a fixture's `.tree` reads as the diagram it describes.
enum Tree {
    static func dump(_ diagram: Diagram) -> String {
        var lines: [String] = []
        switch diagram {
        case .flowchart(let chart):
            lines.append("flowchart \(chart.direction.rawValue)")
            for subgraph in chart.subgraphs {
                lines.append("subgraph \(subgraph.id) \(quoted(subgraph.title))\(subgraph.parent.map { " in \($0)" } ?? "")")
            }
            for node in chart.nodes {
                lines.append("node \(node.id) \(node.shape.rawValue) \(quoted(node.label))\(node.subgraph.map { " in \($0)" } ?? "")")
            }
            for edge in chart.edges {
                lines.append("edge \(edge.from) -> \(edge.to) \(edge.stroke.rawValue) \(edge.tail.rawValue)/\(edge.head.rawValue)"
                    + (edge.label.map { " \(quoted($0))" } ?? ""))
            }
        case .sequence(let sequence):
            lines.append("sequence\(sequence.autonumber ? " autonumber" : "")")
            for participant in sequence.participants {
                lines.append("\(participant.kind.rawValue) \(participant.id) \(quoted(participant.label))")
            }
            lines += items(sequence.items, indent: "")
        case .state(let state):
            lines.append("state \(state.direction.rawValue)")
            for composite in state.composites {
                lines.append("composite \(composite.id) \(quoted(composite.label))\(composite.parent.map { " in \($0)" } ?? "")")
            }
            for item in state.states {
                lines.append("state \(item.id) \(item.kind.rawValue) \(quoted(item.label))\(item.parent.map { " in \($0)" } ?? "")")
            }
            for transition in state.transitions {
                lines.append("transition \(transition.from) -> \(transition.to)" + (transition.label.map { " \(quoted($0))" } ?? ""))
            }
            for note in state.notes {
                lines.append("note \(note.side.rawValue) of \(note.state) \(quoted(note.text))")
            }
        case .classDiagram(let diagram):
            lines.append("class \(diagram.direction.rawValue)")
            for namespace in diagram.namespaces {
                lines.append("namespace \(namespace)")
            }
            for item in diagram.classes {
                lines.append("class \(item.id) \(quoted(item.label))" + (item.annotation.map { " <<\($0)>>" } ?? "")
                    + (item.namespace.map { " in \($0)" } ?? ""))
                lines += item.attributes.map { "  attribute \(quoted($0))" }
                lines += item.methods.map { "  method \(quoted($0))" }
            }
            for relation in diagram.relations {
                lines.append("relation \(relation.from) \(relation.fromEnd.rawValue) \(relation.stroke.rawValue) \(relation.toEnd.rawValue) \(relation.to)"
                    + (relation.fromCardinality.map { " from=\(quoted($0))" } ?? "")
                    + (relation.toCardinality.map { " to=\(quoted($0))" } ?? "")
                    + (relation.label.map { " \(quoted($0))" } ?? ""))
            }
        }
        let accessibility = diagram.accessibility
        if let title = accessibility.title { lines.append("accTitle \(quoted(title))") }
        if let description = accessibility.description { lines.append("accDescr \(quoted(description))") }
        return lines.joined(separator: "\n") + "\n"
    }

    private static func items(_ items: [SequenceDiagram.Item], indent: String) -> [String] {
        items.flatMap { item -> [String] in
            switch item {
            case .message(let message):
                return ["\(indent)message \(message.from) -> \(message.to) \(message.stroke.rawValue) \(message.head.rawValue)"
                    + (message.activation == .none ? "" : " \(message.activation.rawValue)") + " \(quoted(message.text))"]
            case .note(let note):
                return ["\(indent)note \(note.placement.rawValue) \(note.participants.joined(separator: ",")) \(quoted(note.text))"]
            case .activate(let id):
                return ["\(indent)activate \(id)"]
            case .deactivate(let id):
                return ["\(indent)deactivate \(id)"]
            case .block(let block):
                return ["\(indent)\(block.kind.rawValue)"] + block.sections.flatMap { section in
                    ["\(indent)  section \(quoted(section.label))"] + Self.items(section.items, indent: indent + "    ")
                }
            }
        }
    }

    private static func quoted(_ text: String) -> String {
        "\"" + text.replacingOccurrences(of: "\n", with: "\\n") + "\""
    }
}
