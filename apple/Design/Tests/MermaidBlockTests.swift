import AlondraDesign
import AlondraMermaid
import Foundation
import Testing

@Suite struct MermaidBlockTests {
    private static let routing = """
        flowchart TD
            A[Card arrives] --> B{Needs user?}
            B -->|yes| C[Ask Alice]
            B -->|no| D[Run node]
            C --> E[Deliver result]
            D --> E
        """

    @Test func onlyMermaidBlocksAreDrawn() {
        #expect(MermaidBlockView.handles(language: "mermaid"))
        #expect(MermaidBlockView.handles(language: "Mermaid"))
        #expect(!MermaidBlockView.handles(language: "swift"))
        #expect(!MermaidBlockView.handles(language: nil))
    }

    @Test func aValidDiagramIsLaidOut() {
        guard case .diagram(let layout, let summary) = MermaidRendering.make(Self.routing, size: 14) else {
            Issue.record("expected a diagram")
            return
        }
        #expect(layout.nodes.count == 5)
        #expect(summary.kind == .flowchart)
    }

    @Test func aSyntaxErrorNamesItsLine() {
        #expect(MermaidRendering.make("flowchart TD\n    A --> B\n    B -->\n", size: 14) == .notice(.syntax(line: 3)))
    }

    @Test func anUnsupportedKindStaysCode() {
        #expect(MermaidRendering.make("pie title Pets\n    \"Dogs\" : 3\n", size: 14) == .notice(.unsupported))
    }

    @Test func aDiagramOverTheLimitStaysCode() {
        let chain = (0...Mermaid.nodeLimit).map { "    N\($0) --> N\($0 + 1)" }.joined(separator: "\n")
        #expect(MermaidRendering.make("flowchart LR\n\(chain)\n", size: 14) == .notice(.tooLarge))
    }

    @Test func noticesReadInEnglishAndSpanish() throws {
        let english = try Catalog.words("en")
        let spanish = try Catalog.words("es")
        let notices: [(MermaidNotice, String, String)] = [
            (.syntax(line: 3), "This diagram has an error on line 3; showing its code",
             "El diagrama tiene un error en la línea 3; se muestra su código"),
            (.unsupported, "This kind of diagram isn't drawn here yet", "Este tipo de diagrama aún no se dibuja aquí"),
            (.tooLarge, "Diagram too large to draw here", "Diagrama demasiado grande para dibujarlo aquí"),
        ]
        for (notice, en, es) in notices {
            #expect(english(notice.phrase) == en)
            #expect(spanish(notice.phrase) == es)
        }
        #expect(english(.code) == "Code")
        #expect(spanish(.code) == "Código")
        #expect(spanish(.diagram) == "Diagrama")
    }

    @Test func voiceOverReadsAFlowchartInEnglishAndSpanish() throws {
        let summary = try Self.summary(Self.routing)
        let connections = "Card arrives → Needs user?; Needs user? → Ask Alice, yes; Needs user? → Run node, no; "
            + "Ask Alice → Deliver result; Run node → Deliver result"
        #expect(MermaidSpeech(summary, words: try Catalog.words("en")) == MermaidSpeech(label: "Flowchart, 5 steps", value: connections))
        #expect(MermaidSpeech(summary, words: try Catalog.words("es")) == MermaidSpeech(label: "Diagrama de flujo, 5 pasos", value: connections))
    }

    @Test func voiceOverUsesTheDiagramsOwnTitleAndDescription() throws {
        let summary = try Self.summary("""
            flowchart TD
                accTitle: Deploy pipeline
                accDescr: Code is built and tested before it ships.
                Build --> Test --> Ship
            """)
        #expect(MermaidSpeech(summary, words: try Catalog.words("es")) == MermaidSpeech(
            label: "Deploy pipeline", value: "Code is built and tested before it ships.\nBuild → Test; Test → Ship"
        ))
    }

    @Test func voiceOverNamesEveryKindAndCountInEnglishAndSpanish() throws {
        let english = try Catalog.words("en")
        let spanish = try Catalog.words("es")
        let state = try Self.summary("stateDiagram-v2\n    [*] --> Idle\n    Idle --> [*]\n")
        #expect(MermaidSpeech(state, words: english) == MermaidSpeech(label: "State diagram, 1 state", value: "Start → Idle; Idle → End"))
        #expect(MermaidSpeech(state, words: spanish) == MermaidSpeech(label: "Diagrama de estados, 1 estado", value: "Inicio → Idle; Idle → Fin"))

        let sequence = try Self.summary("sequenceDiagram\n    App->>Daemon: Open card\n    Daemon-->>App: Card\n")
        #expect(MermaidSpeech(sequence, words: english)
            == MermaidSpeech(label: "Sequence diagram, 2 participants", value: "App → Daemon, Open card; Daemon → App, Card"))
        #expect(MermaidSpeech(sequence, words: spanish).label == "Diagrama de secuencia, 2 participantes")

        let classes = try Self.summary("classDiagram\n    Task <|-- Request\n    Task *-- Node\n")
        #expect(MermaidSpeech(classes, words: english).label == "Class diagram, 3 classes")
        #expect(MermaidSpeech(classes, words: spanish).label == "Diagrama de clases, 3 clases")
    }

    @Test func voiceOverActionsReadInEnglishAndSpanish() throws {
        #expect(try Catalog.words("en")(.showCode) == "Show code")
        #expect(try Catalog.words("en")(.copyCode) == "Copy code")
        #expect(try Catalog.words("es")(.showCode) == "Ver código")
        #expect(try Catalog.words("es")(.copyCode) == "Copiar código")
    }

    @Test func theCacheKeepsTheLayoutOfTheSameCode() async {
        let cache = MermaidRenderCache()
        let key = MermaidRenderCache.Key(source: Self.routing, size: 14)
        #expect(cache.cached(key) == nil)
        let first = await cache.rendering(key)
        #expect(cache.cached(key) == first)
        #expect(await cache.rendering(key) == first)
        let larger = MermaidRenderCache.Key(source: Self.routing, size: 20)
        #expect(cache.cached(larger) == nil)
        #expect(await cache.rendering(larger) != first)
    }

    @Test func wideDiagramsShrinkButNeverBelowElevenPoints() {
        #expect(MermaidScale.fitting(300, in: 400, textSize: 14) == 1)
        #expect(MermaidScale.fitting(500, in: 400, textSize: 14) == 0.8)
        #expect(MermaidScale.fitting(1000, in: 400, textSize: 14) == 11.0 / 14)
        #expect(MermaidScale.fitting(1000, in: nil, textSize: 14) == 1)
    }

    private static func summary(_ source: String) throws -> MermaidSummary {
        try Mermaid.parse(source).summary
    }
}

/// `AlondraDesign.xcstrings` read the way the app reads its compiled form: a key's plural variation
/// chosen by the phrase's count, the key itself when a language has no entry.
private enum Catalog {
    struct File: Decodable {
        struct Entry: Decodable { var localizations: [String: Localization]? }
        struct Localization: Decodable {
            var stringUnit: Unit?
            var variations: Variations?
        }
        struct Variations: Decodable { var plural: [String: Form]? }
        struct Form: Decodable { var stringUnit: Unit }
        struct Unit: Decodable { var value: String }

        var strings: [String: Entry]
    }

    static func words(_ language: String) throws -> MermaidWords {
        let environment = ProcessInfo.processInfo.environment
        let root = try #require(environment["TEST_SRCDIR"])
        let url = URL(fileURLWithPath: root)
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("apple/Design/Resources/AlondraDesign.xcstrings")
        let file = try JSONDecoder().decode(File.self, from: Data(contentsOf: url))
        return MermaidWords(locale: Locale(identifier: language)) { phrase in
            let localization = file.strings[phrase.key]?.localizations?[language]
            if let plural = localization?.variations?.plural {
                return plural[phrase.count == 1 ? "one" : "other"]?.stringUnit.value ?? phrase.key
            }
            return localization?.stringUnit?.value ?? phrase.key
        }
    }
}
