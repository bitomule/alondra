import AlondraMermaid
import Foundation

/// A sentence a Mermaid block shows or speaks, as its catalog key and arguments, so tests can read
/// it in every language the catalog has.
public struct MermaidPhrase: Sendable, Equatable {
    public enum Argument: Sendable, Equatable {
        case number(Int)
        case text(String)

        var formatted: any CVarArg {
            switch self {
            case .number(let value): value
            case .text(let value): value
            }
        }
    }

    public let key: String
    public let arguments: [Argument]

    init(_ key: String, _ arguments: Argument...) {
        self.key = key
        self.arguments = arguments
    }

    /// The count a plural variation is chosen by.
    public var count: Int? {
        arguments.lazy.compactMap { argument -> Int? in
            if case .number(let value) = argument { return value }
            return nil
        }.first
    }

    static func syntaxError(line: Int) -> MermaidPhrase { MermaidPhrase("This diagram has an error on line %lld; showing its code", .number(line)) }
    static let unsupported = MermaidPhrase("This kind of diagram isn't drawn here yet")
    static let tooLarge = MermaidPhrase("Diagram too large to draw here")
    public static let code = MermaidPhrase("Code")
    public static let diagram = MermaidPhrase("Diagram")
    public static let showCode = MermaidPhrase("Show code")
    public static let copyCode = MermaidPhrase("Copy code")
    static let start = MermaidPhrase("Start")
    static let end = MermaidPhrase("End")

    static func count(_ kind: MermaidSummary.Kind, _ count: Int) -> MermaidPhrase {
        switch kind {
        case .flowchart: MermaidPhrase("Flowchart, %lld steps", .number(count))
        case .sequence: MermaidPhrase("Sequence diagram, %lld participants", .number(count))
        case .state: MermaidPhrase("State diagram, %lld states", .number(count))
        case .classDiagram: MermaidPhrase("Class diagram, %lld classes", .number(count))
        }
    }
}

/// Turns phrases into text with a catalog: the app's, or one a test reads from the `.xcstrings` file.
public struct MermaidWords: Sendable {
    private let locale: Locale
    private let format: @Sendable (MermaidPhrase) -> String

    public init(locale: Locale, format: @escaping @Sendable (MermaidPhrase) -> String) {
        self.locale = locale
        self.format = format
    }

    public static var app: MermaidWords {
        MermaidWords(locale: .current) { phrase in
            #bundle.localizedString(forKey: phrase.key, value: phrase.key, table: "AlondraDesign")
        }
    }

    public func callAsFunction(_ phrase: MermaidPhrase) -> String {
        String(format: format(phrase), locale: locale, arguments: phrase.arguments.map(\.formatted))
    }
}

/// What VoiceOver reads for a diagram: its title, or its kind and size; then its description and
/// every connection in order.
public struct MermaidSpeech: Sendable, Equatable {
    public let label: String
    public let value: String

    public init(label: String, value: String) {
        self.label = label
        self.value = value
    }

    public init(_ summary: MermaidSummary, words: MermaidWords) {
        label = summary.title ?? words(.count(summary.kind, summary.nodeCount))
        func name(_ endpoint: MermaidSummary.Endpoint) -> String {
            switch endpoint {
            case .node(let name): name
            case .start: words(.start)
            case .end: words(.end)
            }
        }
        let connections = summary.connections.map { connection in
            let arrow = "\(name(connection.from)) → \(name(connection.to))"
            return connection.label.map { "\(arrow), \($0)" } ?? arrow
        }
        .joined(separator: "; ")
        value = [summary.description, connections.isEmpty ? nil : connections]
            .compactMap { $0 }
            .joined(separator: "\n")
    }
}
