import CoreGraphics
import Foundation
import os

extension Mermaid {
    static let signposter = OSSignposter(subsystem: "com.bitomule.alondra.mermaid", category: "layout")

    /// Places a parsed diagram with the theme's fonts. Pure and `Sendable`: run it off the main
    /// thread and keep the result; a colour change redraws the same layout.
    public static func layout(_ diagram: Diagram, typography: MermaidTypography) -> MermaidLayout {
        let state = signposter.beginInterval("layout", id: signposter.makeSignpostID())
        defer { signposter.endInterval("layout", state) }
        let measure = Measure(typography: typography)
        switch diagram {
        case .flowchart(let chart): return GraphScene.flowchart(chart, measure: measure)
        case .state(let state): return GraphScene.state(state, measure: measure)
        case .classDiagram(let classes): return GraphScene.classDiagram(classes, measure: measure)
        case .sequence(let sequence): return SequenceLayout(sequence, measure: measure).layout()
        }
    }
}

/// Everything a diagram draws, in drawing order, plus the geometry tests and callers read.
public struct MermaidLayout: Sendable, Equatable {
    public enum Paint: Sendable, Equatable {
        case text, mutedText, nodeFill, nodeStroke, edge, labelBackground, clusterFill, clusterStroke
        case noteFill, noteStroke, accent, clear
    }

    public enum TextRole: Sendable, Equatable {
        case label, strong, edgeLabel, title
    }

    public enum Outline: Sendable, Equatable {
        case rectangle, rounded, stadium, subroutine, cylinder, circle, doubleCircle, diamond, hexagon, note, bar
    }

    public enum Dash: Sendable, Equatable {
        case solid, dashed, dotted, thick
    }

    public enum Marker: Sendable, Equatable {
        case none, arrow, openArrow, cross, circle, triangle, filledDiamond, diamond
    }

    public struct Shape: Sendable, Equatable {
        public var outline: Outline
        public var frame: CGRect
        public var fill: Paint
        public var stroke: Paint
        public var dash: Dash
    }

    public struct Line: Sendable, Equatable {
        public var points: [CGPoint]
        public var curved: Bool
        public var paint: Paint
        public var dash: Dash
        public var start: Marker
        public var end: Marker
    }

    public struct Text: Sendable, Equatable {
        public enum Alignment: Sendable, Equatable {
            case center, leading
        }

        public var lines: [String]
        /// The centre of the block, or the middle of its left edge when `leading`.
        public var position: CGPoint
        public var alignment: Alignment
        public var role: TextRole
        public var paint: Paint
        public var lineHeight: CGFloat
        /// Painted behind the text, e.g. so an edge label covers its line.
        public var background: CGRect?
    }

    public enum Item: Sendable, Equatable {
        case shape(Shape)
        case line(Line)
        case text(Text)
        case actor(CGRect, Paint)
    }

    public struct PlacedNode: Sendable, Equatable {
        public var id: String
        public var frame: CGRect
        public var outline: Outline
    }

    public struct PlacedEdge: Sendable, Equatable {
        public var from: String
        public var to: String
        /// From the `from` node's border to the `to` node's border.
        public var points: [CGPoint]
        /// The edge was turned around to break a cycle, so it runs against the diagram's direction.
        public var reversed: Bool
        public var labelFrame: CGRect?
    }

    public struct PlacedCluster: Sendable, Equatable {
        public var id: String
        public var frame: CGRect
        /// Every node inside, nested clusters included.
        public var members: [String]
    }

    public struct PlacedMessage: Sendable, Equatable {
        public var from: String
        public var to: String
        public var y: CGFloat
    }

    public struct PlacedBlock: Sendable, Equatable {
        public var frame: CGRect
        /// Indexes into `messages` of the messages inside.
        public var messages: [Int]
    }

    public var size: CGSize
    public var items: [Item]
    public var nodes: [PlacedNode] = []
    public var edges: [PlacedEdge] = []
    public var clusters: [PlacedCluster] = []
    /// Edge crossings between neighbouring rows of the final placement.
    public var crossings = 0
    /// Sequence diagrams: each participant's lifeline x, in declaration order.
    public var lanes: [CGFloat] = []
    public var messages: [PlacedMessage] = []
    public var blocks: [PlacedBlock] = []
}
