import AlondraMermaid
import Testing

@Suite struct SummaryTests {
    @Test func flowchartListsItsEdgesWithLabels() throws {
        let summary = try Corpus.diagram("flow_card_routing").summary
        #expect(summary == MermaidSummary(
            kind: .flowchart, title: nil, description: nil, nodeCount: 5,
            connections: [
                .init(from: .node("Card arrives"), to: .node("Needs user?"), label: nil),
                .init(from: .node("Needs user?"), to: .node("Ask Alice"), label: "yes"),
                .init(from: .node("Needs user?"), to: .node("Run node"), label: "no"),
                .init(from: .node("Ask Alice"), to: .node("Deliver result"), label: nil),
                .init(from: .node("Run node"), to: .node("Deliver result"), label: nil),
            ]
        ))
    }

    @Test func sequenceListsItsMessagesInOrder() throws {
        let summary = try Corpus.diagram("seq_critical_break").summary
        #expect(summary == MermaidSummary(
            kind: .sequence, title: "Connection with fallback", description: "The app tries the relay when the direct link fails.",
            nodeCount: 3,
            connections: [
                .init(from: .node("App"), to: .node("Mac"), label: "Hello"),
                .init(from: .node("App"), to: .node("Relay"), label: "Hello through relay"),
                .init(from: .node("Mac"), to: .node("App"), label: "Sign in again"),
            ]
        ))
    }

    @Test func stateNamesItsStartAndEnd() throws {
        let summary = try Corpus.diagram("state_basic").summary
        #expect(summary.nodeCount == 3)
        #expect(summary.connections.first == .init(from: .start, to: .node("Still"), label: nil))
        #expect(summary.connections.last == .init(from: .node("Crash"), to: .end, label: nil))
    }

    @Test func accessibilityTitleAndDescriptionComeThrough() throws {
        let summary = try Corpus.diagram("flow_accessibility").summary
        #expect(summary.title == "Deploy pipeline")
        #expect(summary.description == "Code goes through build and test before it ships.")
    }
}
