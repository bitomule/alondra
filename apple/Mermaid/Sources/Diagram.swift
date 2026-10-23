import Foundation

/// Reads the text of a Mermaid block into one of the four diagrams Alondra draws, and lays it out.
public enum Mermaid {
    /// A diagram with more nodes than this is refused before it is laid out.
    public static let nodeLimit = 100

    public static func parse(_ source: String) throws(MermaidError) -> Diagram {
        let document = Document(source)
        let header = try Header.read(document)
        let diagram: Diagram
        switch header.kind {
        case .flowchart:
            var parser = FlowchartParser(document: document, header: header)
            diagram = .flowchart(try parser.parse())
        case .sequence:
            var parser = SequenceParser(document: document, header: header)
            diagram = .sequence(try parser.parse())
        case .state:
            var parser = StateParser(document: document, header: header)
            diagram = .state(try parser.parse())
        case .classDiagram:
            var parser = ClassParser(document: document, header: header)
            diagram = .classDiagram(try parser.parse())
        }
        if diagram.nodeCount > nodeLimit {
            throw .tooLarge(nodes: diagram.nodeCount)
        }
        return diagram
    }
}

public enum MermaidError: Error, Equatable, Sendable {
    /// The first place the text stops being a diagram, 1-based. `expected` names what the parser
    /// looked for there; it is for logs and tests, never shown to the user.
    case syntax(line: Int, column: Int, expected: String)
    /// The header names a diagram this module does not draw (`pie`, `gantt`…).
    case unsupported(kind: String)
    case tooLarge(nodes: Int)
}

public enum Direction: String, Sendable, Equatable {
    case topToBottom = "TB"
    case bottomToTop = "BT"
    case leftToRight = "LR"
    case rightToLeft = "RL"

    init?(keyword: String) {
        switch keyword.uppercased() {
        case "TB", "TD": self = .topToBottom
        case "BT": self = .bottomToTop
        case "LR": self = .leftToRight
        case "RL": self = .rightToLeft
        default: return nil
        }
    }

    var isVertical: Bool { self == .topToBottom || self == .bottomToTop }
}

/// `accTitle`, `accDescr`, or the front matter `title`.
public struct Accessibility: Sendable, Equatable {
    public var title: String?
    public var description: String?

    public init(title: String? = nil, description: String? = nil) {
        self.title = title
        self.description = description
    }
}

public enum Diagram: Sendable, Equatable {
    case flowchart(Flowchart)
    case sequence(SequenceDiagram)
    case state(StateDiagram)
    case classDiagram(ClassDiagram)

    public var accessibility: Accessibility {
        switch self {
        case .flowchart(let chart): chart.accessibility
        case .sequence(let sequence): sequence.accessibility
        case .state(let state): state.accessibility
        case .classDiagram(let diagram): diagram.accessibility
        }
    }

    /// What the node limit counts: nodes, states or classes; participants and messages in a sequence.
    public var nodeCount: Int {
        switch self {
        case .flowchart(let chart): chart.nodes.count
        case .sequence(let sequence): sequence.participants.count + sequence.messages.count
        case .state(let state): state.states.count + state.composites.count
        case .classDiagram(let diagram): diagram.classes.count
        }
    }
}

public struct Flowchart: Sendable, Equatable {
    public var direction: Direction
    public var nodes: [Node]
    public var edges: [Edge]
    public var subgraphs: [Subgraph]
    public var accessibility: Accessibility

    public enum Shape: String, Sendable, Equatable {
        case rectangle, rounded, stadium, subroutine, cylinder, circle, diamond, hexagon
    }

    public struct Node: Sendable, Equatable {
        public var id: String
        public var label: String
        public var shape: Shape
        /// The innermost subgraph the node was first mentioned in.
        public var subgraph: String?
    }

    public enum Stroke: String, Sendable, Equatable {
        case solid, dotted, thick
    }

    public enum Head: String, Sendable, Equatable {
        case none, arrow, cross, circle
    }

    /// `from` or `to` may name a subgraph: the edge then reaches the subgraph's box.
    public struct Edge: Sendable, Equatable {
        public var from: String
        public var to: String
        public var label: String?
        public var stroke: Stroke
        public var head: Head
        public var tail: Head
    }

    public struct Subgraph: Sendable, Equatable {
        public var id: String
        public var title: String
        public var parent: String?
    }
}

public struct SequenceDiagram: Sendable, Equatable {
    public var participants: [Participant]
    public var items: [Item]
    public var autonumber: Bool
    public var accessibility: Accessibility

    public struct Participant: Sendable, Equatable {
        public enum Kind: String, Sendable, Equatable {
            case participant, actor
        }

        public var id: String
        public var label: String
        public var kind: Kind
    }

    public enum Stroke: String, Sendable, Equatable {
        case solid, dotted
    }

    public enum Head: String, Sendable, Equatable {
        case none, arrow, cross, open
    }

    public enum Activation: String, Sendable, Equatable {
        case none, activateTarget, deactivateSource
    }

    public struct Message: Sendable, Equatable {
        public var from: String
        public var to: String
        public var text: String
        public var stroke: Stroke
        public var head: Head
        public var activation: Activation
    }

    public struct Note: Sendable, Equatable {
        public enum Placement: String, Sendable, Equatable {
            case leftOf, rightOf, over
        }

        public var placement: Placement
        public var participants: [String]
        public var text: String
    }

    public struct Block: Sendable, Equatable {
        public enum Kind: String, Sendable, Equatable {
            case loop, alt, opt, par, critical, `break`
        }

        public var kind: Kind
        public var sections: [Section]
    }

    /// The first section carries the block's own label; the others an `else`, `and` or `option`.
    public struct Section: Sendable, Equatable {
        public var label: String
        public var items: [Item]
    }

    public enum Item: Sendable, Equatable {
        case message(Message)
        case note(Note)
        case activate(String)
        case deactivate(String)
        case block(Block)
    }

    /// Every message, nested blocks included, in the order they are read.
    public var messages: [Message] {
        func collect(_ items: [Item]) -> [Message] {
            items.flatMap { item -> [Message] in
                switch item {
                case .message(let message): [message]
                case .block(let block): block.sections.flatMap { collect($0.items) }
                case .note, .activate, .deactivate: []
                }
            }
        }
        return collect(items)
    }
}

public struct StateDiagram: Sendable, Equatable {
    public var direction: Direction
    public var states: [State]
    public var transitions: [Transition]
    public var composites: [Composite]
    public var notes: [Note]
    public var accessibility: Accessibility

    public enum Kind: String, Sendable, Equatable {
        case normal, start, end, choice, fork, join
    }

    public struct State: Sendable, Equatable {
        public var id: String
        public var label: String
        public var kind: Kind
        public var parent: String?
    }

    /// `from` or `to` may name a composite state: the transition enters its start or leaves its end.
    public struct Transition: Sendable, Equatable {
        public var from: String
        public var to: String
        public var label: String?
    }

    public struct Composite: Sendable, Equatable {
        public var id: String
        public var label: String
        public var parent: String?
    }

    public struct Note: Sendable, Equatable {
        public enum Side: String, Sendable, Equatable {
            case left, right
        }

        public var state: String
        public var side: Side
        public var text: String
    }
}

public struct ClassDiagram: Sendable, Equatable {
    public var direction: Direction
    public var classes: [Class]
    public var relations: [Relation]
    public var namespaces: [String]
    public var accessibility: Accessibility

    public struct Class: Sendable, Equatable {
        public var id: String
        public var label: String
        public var annotation: String?
        public var attributes: [String]
        public var methods: [String]
        public var namespace: String?
    }

    public enum End: String, Sendable, Equatable {
        case none, inheritance, composition, aggregation, arrow
    }

    public enum Stroke: String, Sendable, Equatable {
        case solid, dashed
    }

    public struct Relation: Sendable, Equatable {
        public var from: String
        public var to: String
        public var fromEnd: End
        public var toEnd: End
        public var stroke: Stroke
        public var fromCardinality: String?
        public var toCardinality: String?
        public var label: String?
    }
}
