/// What VoiceOver reads for a diagram, as data: the app turns it into translated sentences.
public struct MermaidSummary: Sendable, Equatable {
    public enum Kind: Sendable, Equatable {
        case flowchart, sequence, state, classDiagram
    }

    public enum Endpoint: Sendable, Equatable {
        case node(String)
        /// A state diagram's `[*]` as a source.
        case start
        /// A state diagram's `[*]` as a target.
        case end
    }

    /// An edge, transition, relation or message, with the labels of its ends.
    public struct Connection: Sendable, Equatable {
        public var from: Endpoint
        public var to: Endpoint
        public var label: String?

        public init(from: Endpoint, to: Endpoint, label: String?) {
            self.from = from
            self.to = to
            self.label = label
        }
    }

    public var kind: Kind
    public var title: String?
    public var description: String?
    /// Nodes, participants, states or classes.
    public var nodeCount: Int
    /// In the order the diagram declares them; messages in the order they are sent.
    public var connections: [Connection]

    public init(kind: Kind, title: String?, description: String?, nodeCount: Int, connections: [Connection]) {
        self.kind = kind
        self.title = title
        self.description = description
        self.nodeCount = nodeCount
        self.connections = connections
    }
}

extension Diagram {
    public var summary: MermaidSummary {
        func spoken(_ text: String) -> String {
            text.split(separator: "\n").joined(separator: " ")
        }
        func optional(_ text: String?) -> String? {
            guard let text, !text.isEmpty else { return nil }
            return spoken(text)
        }
        let accessibility = self.accessibility
        switch self {
        case .flowchart(let chart):
            var names = Dictionary(uniqueKeysWithValues: chart.subgraphs.map { ($0.id, $0.title) })
            for node in chart.nodes { names[node.id] = node.label }
            return MermaidSummary(
                kind: .flowchart, title: accessibility.title, description: accessibility.description, nodeCount: chart.nodes.count,
                connections: chart.edges.map { edge in
                    .init(from: .node(spoken(names[edge.from] ?? edge.from)), to: .node(spoken(names[edge.to] ?? edge.to)),
                          label: optional(edge.label))
                }
            )
        case .sequence(let sequence):
            let names = Dictionary(uniqueKeysWithValues: sequence.participants.map { ($0.id, $0.label) })
            return MermaidSummary(
                kind: .sequence, title: accessibility.title, description: accessibility.description,
                nodeCount: sequence.participants.count,
                connections: sequence.messages.map { message in
                    .init(from: .node(spoken(names[message.from] ?? message.from)), to: .node(spoken(names[message.to] ?? message.to)),
                          label: optional(message.text))
                }
            )
        case .state(let diagram):
            var names = Dictionary(uniqueKeysWithValues: diagram.composites.map { ($0.id, $0.label) })
            var kinds: [String: StateDiagram.Kind] = [:]
            for state in diagram.states {
                names[state.id] = state.label.isEmpty ? state.id : state.label
                kinds[state.id] = state.kind
            }
            func endpoint(_ id: String) -> MermaidSummary.Endpoint {
                switch kinds[id] {
                case .start: .start
                case .end: .end
                default: .node(spoken(names[id] ?? id))
                }
            }
            let real = diagram.states.filter { $0.kind != .start && $0.kind != .end }.count
            return MermaidSummary(
                kind: .state, title: accessibility.title, description: accessibility.description,
                nodeCount: real + diagram.composites.count,
                connections: diagram.transitions.map { .init(from: endpoint($0.from), to: endpoint($0.to), label: optional($0.label)) }
            )
        case .classDiagram(let diagram):
            let names = Dictionary(uniqueKeysWithValues: diagram.classes.map { ($0.id, $0.label) })
            return MermaidSummary(
                kind: .classDiagram, title: accessibility.title, description: accessibility.description,
                nodeCount: diagram.classes.count,
                connections: diagram.relations.map { relation in
                    .init(from: .node(names[relation.from] ?? relation.from), to: .node(names[relation.to] ?? relation.to),
                          label: optional(relation.label))
                }
            )
        }
    }
}
