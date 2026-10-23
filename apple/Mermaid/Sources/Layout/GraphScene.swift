import CoreGraphics

/// Turns a flowchart, state or class diagram into a `LayeredGraph`, places it and lists what to draw.
enum GraphScene {
    struct NodeSpec {
        enum Content {
            case label(Measure.Block)
            case members(header: [Measure.Block], sections: [[Measure.Block]], headerHeight: CGFloat, sectionHeights: [CGFloat])
            case empty
        }

        var id: String
        var size: CGSize
        var outline: MermaidLayout.Outline
        var cluster: Int?
        var fill: MermaidLayout.Paint
        var stroke: MermaidLayout.Paint
        var dash: MermaidLayout.Dash = .solid
        var content: Content
        var leadingNote: Measure.Block? = nil
        var trailingNote: Measure.Block? = nil
    }

    struct EdgeSpec {
        var from: Int
        var to: Int
        var fromID: String
        var toID: String
        var label: Measure.Block?
        var dash: MermaidLayout.Dash
        var start: MermaidLayout.Marker
        var end: MermaidLayout.Marker
        var startText: Measure.Block? = nil
        var endText: Measure.Block? = nil
    }

    struct ClusterSpec {
        var id: String
        var parent: Int?
        var title: Measure.Block
    }

    static let labelPadding = CGSize(width: 8, height: 3)

    // MARK: Flowchart

    static func flowchart(_ chart: Flowchart, measure: Measure) -> MermaidLayout {
        let clusterIndex = Dictionary(uniqueKeysWithValues: chart.subgraphs.enumerated().map { ($1.id, $0) })
        let clusters = chart.subgraphs.map { subgraph in
            ClusterSpec(id: subgraph.id, parent: subgraph.parent.flatMap { clusterIndex[$0] }, title: measure.text(subgraph.title, .title))
        }
        let nodes = chart.nodes.map { node -> NodeSpec in
            let text = measure.text(node.label, .label)
            return NodeSpec(
                id: node.id, size: size(of: node.shape, text: text.size), outline: outline(node.shape),
                cluster: node.subgraph.flatMap { clusterIndex[$0] }, fill: .nodeFill, stroke: .nodeStroke, content: .label(text)
            )
        }
        let nodeIndex = Dictionary(uniqueKeysWithValues: nodes.enumerated().map { ($1.id, $0) })
        let ancestry = clusters.indices.map { index -> [Int] in
            var path: [Int] = []
            var current: Int? = index
            while let cluster = current {
                path.append(cluster)
                current = clusters[cluster].parent
            }
            return path
        }
        func inside(_ cluster: Int) -> [Int] {
            nodes.indices.filter { node in nodes[node].cluster.map { ancestry[$0].contains(cluster) } ?? false }
        }
        func endpoint(_ id: String, entering: Bool) -> Int? {
            if let node = nodeIndex[id] { return node }
            guard let cluster = clusterIndex[id] else { return nil }
            return entering ? inside(cluster).first : inside(cluster).last
        }
        let edges = chart.edges.compactMap { edge -> EdgeSpec? in
            guard let from = endpoint(edge.from, entering: false), let to = endpoint(edge.to, entering: true) else { return nil }
            return EdgeSpec(
                from: from, to: to, fromID: edge.from, toID: edge.to,
                label: edge.label.map { measure.text($0, .edgeLabel) },
                dash: dash(edge.stroke), start: marker(edge.tail), end: marker(edge.head)
            )
        }
        return assemble(direction: chart.direction, nodes: nodes, edges: edges, clusters: clusters)
    }

    static func size(of shape: Flowchart.Shape, text: CGSize) -> CGSize {
        let width = text.width + 28
        let height = text.height + 18
        switch shape {
        case .rectangle, .rounded: return CGSize(width: max(width, 48), height: height)
        case .stadium: return CGSize(width: width + height / 2, height: height)
        case .subroutine: return CGSize(width: width + 16, height: height)
        case .cylinder: return CGSize(width: max(width, 48), height: height + 14)
        case .hexagon: return CGSize(width: width + height / 2, height: height)
        case .circle:
            let diameter = ((text.width * text.width + text.height * text.height).squareRoot() + 16).rounded(.up)
            return CGSize(width: diameter, height: diameter)
        case .diamond:
            let half = ((text.width + 12) + 1.5 * (text.height + 8)) / 2
            return CGSize(width: (2 * half).rounded(.up), height: (2 * half / 1.5).rounded(.up))
        }
    }

    private static func outline(_ shape: Flowchart.Shape) -> MermaidLayout.Outline {
        switch shape {
        case .rectangle: .rectangle
        case .rounded: .rounded
        case .stadium: .stadium
        case .subroutine: .subroutine
        case .cylinder: .cylinder
        case .circle: .circle
        case .diamond: .diamond
        case .hexagon: .hexagon
        }
    }

    private static func dash(_ stroke: Flowchart.Stroke) -> MermaidLayout.Dash {
        switch stroke {
        case .solid: .solid
        case .dotted: .dotted
        case .thick: .thick
        }
    }

    private static func marker(_ head: Flowchart.Head) -> MermaidLayout.Marker {
        switch head {
        case .none: .none
        case .arrow: .arrow
        case .cross: .cross
        case .circle: .circle
        }
    }

    // MARK: State

    static func state(_ diagram: StateDiagram, measure: Measure) -> MermaidLayout {
        let clusterIndex = Dictionary(uniqueKeysWithValues: diagram.composites.enumerated().map { ($1.id, $0) })
        let clusters = diagram.composites.map { composite in
            ClusterSpec(id: composite.id, parent: composite.parent.flatMap { clusterIndex[$0] }, title: measure.text(composite.label, .title))
        }
        var nodes = diagram.states.map { state -> NodeSpec in
            let cluster = state.parent.flatMap { clusterIndex[$0] }
            switch state.kind {
            case .start:
                return NodeSpec(id: state.id, size: CGSize(width: 16, height: 16), outline: .circle, cluster: cluster,
                                fill: .accent, stroke: .accent, content: .empty)
            case .end:
                return NodeSpec(id: state.id, size: CGSize(width: 20, height: 20), outline: .doubleCircle, cluster: cluster,
                                fill: .accent, stroke: .accent, content: .empty)
            case .choice:
                return NodeSpec(id: state.id, size: CGSize(width: 28, height: 28), outline: .diamond, cluster: cluster,
                                fill: .nodeFill, stroke: .nodeStroke, content: .empty)
            case .fork, .join:
                let size = diagram.direction.isVertical ? CGSize(width: 72, height: 8) : CGSize(width: 8, height: 72)
                return NodeSpec(id: state.id, size: size, outline: .bar, cluster: cluster, fill: .accent, stroke: .accent,
                                content: .empty)
            case .normal:
                let text = measure.text(state.label, .label)
                return NodeSpec(id: state.id, size: CGSize(width: max(text.size.width + 28, 56), height: text.size.height + 16),
                                outline: .rounded, cluster: cluster, fill: .nodeFill, stroke: .nodeStroke, content: .label(text))
            }
        }
        let nodeIndex = Dictionary(uniqueKeysWithValues: nodes.enumerated().map { ($1.id, $0) })
        func within(_ state: StateDiagram.State, _ composite: String) -> Bool {
            var parent = state.parent
            while let current = parent {
                if current == composite { return true }
                parent = diagram.composites.first { $0.id == current }?.parent
            }
            return false
        }
        func endpoint(_ id: String, entering: Bool) -> Int? {
            if let node = nodeIndex[id] { return node }
            guard clusterIndex[id] != nil else { return nil }
            if let pseudo = nodeIndex[(entering ? "[*]start@" : "[*]end@") + id] { return pseudo }
            let inside = diagram.states.indices.filter { within(diagram.states[$0], id) }
            return entering ? inside.first : inside.last
        }
        let edges = diagram.transitions.compactMap { transition -> EdgeSpec? in
            guard let from = endpoint(transition.from, entering: false), let to = endpoint(transition.to, entering: true) else {
                return nil
            }
            return EdgeSpec(from: from, to: to, fromID: transition.from, toID: transition.to,
                            label: transition.label.map { measure.text($0, .edgeLabel) }, dash: .solid, start: .none, end: .arrow)
        }
        for note in diagram.notes {
            guard let state = nodeIndex[note.state] else { continue }
            let text = measure.text(note.text, .label)
            if note.side == .left { nodes[state].leadingNote = text } else { nodes[state].trailingNote = text }
        }
        return assemble(direction: diagram.direction, nodes: nodes, edges: edges, clusters: clusters)
    }

    // MARK: Class

    static func classDiagram(_ diagram: ClassDiagram, measure: Measure) -> MermaidLayout {
        let clusterIndex = Dictionary(uniqueKeysWithValues: diagram.namespaces.enumerated().map { ($1, $0) })
        let clusters = diagram.namespaces.map { ClusterSpec(id: $0, parent: nil, title: measure.text($0, .title)) }
        let nodes = diagram.classes.map { item -> NodeSpec in
            var header: [Measure.Block] = []
            if let annotation = item.annotation {
                header.append(measure.text("«\(annotation)»", .edgeLabel))
            }
            header.append(measure.text(item.label, .strong))
            let sections = [item.attributes, item.methods].map { lines in lines.map { measure.text($0, .label) } }
            let headerHeight = header.map(\.size.height).reduce(0, +) + 14
            let sectionHeights = sections.map { section in max(section.map(\.size.height).reduce(0, +) + 10, 12) }
            let widest = (header + sections.flatMap { $0 }).map(\.size.width).max() ?? 0
            let size = CGSize(width: max(widest + 28, 80), height: headerHeight + sectionHeights.reduce(0, +))
            return NodeSpec(
                id: item.id, size: size, outline: .rectangle, cluster: item.namespace.flatMap { clusterIndex[$0] },
                fill: .nodeFill, stroke: .nodeStroke,
                content: .members(header: header, sections: sections, headerHeight: headerHeight, sectionHeights: sectionHeights)
            )
        }
        let nodeIndex = Dictionary(uniqueKeysWithValues: nodes.enumerated().map { ($1.id, $0) })
        let edges = diagram.relations.compactMap { relation -> EdgeSpec? in
            guard let from = nodeIndex[relation.from], let to = nodeIndex[relation.to] else { return nil }
            return EdgeSpec(
                from: from, to: to, fromID: relation.from, toID: relation.to,
                label: relation.label.map { measure.text($0, .edgeLabel) },
                dash: relation.stroke == .dashed ? .dashed : .solid,
                start: marker(relation.fromEnd), end: marker(relation.toEnd),
                startText: relation.fromCardinality.map { measure.text($0, .edgeLabel) },
                endText: relation.toCardinality.map { measure.text($0, .edgeLabel) }
            )
        }
        return assemble(direction: diagram.direction, nodes: nodes, edges: edges, clusters: clusters)
    }

    private static func marker(_ end: ClassDiagram.End) -> MermaidLayout.Marker {
        switch end {
        case .none: .none
        case .inheritance: .triangle
        case .composition: .filledDiamond
        case .aggregation: .diamond
        case .arrow: .openArrow
        }
    }

    // MARK: Assembly

    static func assemble(direction: Direction, nodes: [NodeSpec], edges: [EdgeSpec], clusters: [ClusterSpec]) -> MermaidLayout {
        let graph = LayeredGraph(
            direction: direction,
            nodes: nodes.map { node in
                LayeredGraph.Node(
                    size: node.size, cluster: node.cluster,
                    leading: node.leadingNote.map { noteSize($0) }, trailing: node.trailingNote.map { noteSize($0) }
                )
            },
            edges: edges.map { edge in
                LayeredGraph.Edge(from: edge.from, to: edge.to, label: edge.label.map { labelBox($0.size) })
            },
            clusters: clusters.map { LayeredGraph.Cluster(parent: $0.parent, title: $0.title.size) }
        )
        let placed = Layered.place(graph)
        var items: [MermaidLayout.Item] = []

        let depth = clusters.indices.map { index -> Int in
            var count = 0
            var current = clusters[index].parent
            while let parent = current {
                count += 1
                current = clusters[parent].parent
            }
            return count
        }
        var placedClusters: [MermaidLayout.PlacedCluster] = []
        for index in clusters.indices.sorted(by: { depth[$0] < depth[$1] }) where !placed.clusters[index].isNull {
            let frame = placed.clusters[index]
            items.append(.shape(.init(outline: .rectangle, frame: frame, fill: .clusterFill, stroke: .clusterStroke, dash: .solid)))
            let title = clusters[index].title
            items.append(.text(.init(
                lines: title.lines,
                position: CGPoint(x: frame.minX + Layered.clusterPad / 2 + 2, y: frame.minY + Layered.titleGap / 2 + 2 + title.size.height / 2),
                alignment: .leading, role: .title, paint: .mutedText, lineHeight: title.lineHeight, background: nil
            )))
        }
        for index in clusters.indices where !placed.clusters[index].isNull {
            func contains(_ cluster: Int?) -> Bool {
                var current = cluster
                while let value = current {
                    if value == index { return true }
                    current = clusters[value].parent
                }
                return false
            }
            placedClusters.append(.init(
                id: clusters[index].id, frame: placed.clusters[index],
                members: nodes.filter { contains($0.cluster) }.flatMap { node in
                    [node.id] + (node.leadingNote == nil ? [] : ["note:left:\(node.id)"])
                        + (node.trailingNote == nil ? [] : ["note:right:\(node.id)"])
                }
            ))
        }

        var placedEdges: [MermaidLayout.PlacedEdge] = []
        var labels: [MermaidLayout.Item] = []
        for (index, edge) in edges.enumerated() {
            let route = placed.routes[index]
            var points = route.points
            if !route.selfLoop, points.count >= 2 {
                let fromFrame = placed.nodes[edge.from]
                let toFrame = placed.nodes[edge.to]
                points[0] = Boundary.clip(from: points[0], toward: points[1], frame: fromFrame, outline: nodes[edge.from].outline)
                let last = points.count - 1
                points[last] = Boundary.clip(from: points[last], toward: points[last - 1], frame: toFrame, outline: nodes[edge.to].outline)
            }
            items.append(.line(.init(points: points, curved: true, paint: .edge, dash: edge.dash, start: edge.start, end: edge.end)))
            var labelFrame: CGRect?
            if let label = edge.label, let center = route.labelCenter {
                let box = labelBox(label.size)
                let frame = CGRect(x: center.x - box.width / 2, y: center.y - box.height / 2, width: box.width, height: box.height)
                labelFrame = frame
                labels.append(.text(.init(
                    lines: label.lines, position: center, alignment: .center, role: .edgeLabel, paint: .text,
                    lineHeight: label.lineHeight, background: frame
                )))
            }
            if let text = edge.startText, points.count >= 2 {
                labels.append(endText(text, at: points[0], toward: points[1]))
            }
            if let text = edge.endText, points.count >= 2 {
                labels.append(endText(text, at: points[points.count - 1], toward: points[points.count - 2]))
            }
            placedEdges.append(.init(from: edge.fromID, to: edge.toID, points: points, reversed: route.reversed, labelFrame: labelFrame))
        }

        var placedNodes: [MermaidLayout.PlacedNode] = []
        for (index, node) in nodes.enumerated() {
            let frame = placed.nodes[index]
            placedNodes.append(.init(id: node.id, frame: frame, outline: node.outline))
            for (text, noteFrame, side) in [(node.leadingNote, placed.leading[index], "left"), (node.trailingNote, placed.trailing[index], "right")] {
                guard let text, let noteFrame else { continue }
                placedNodes.append(.init(id: "note:\(side):\(node.id)", frame: noteFrame, outline: .note))
                let from = Boundary.clip(from: CGPoint(x: frame.midX, y: frame.midY), toward: CGPoint(x: noteFrame.midX, y: noteFrame.midY),
                                         frame: frame, outline: node.outline)
                let to = Boundary.clip(from: CGPoint(x: noteFrame.midX, y: noteFrame.midY), toward: CGPoint(x: frame.midX, y: frame.midY),
                                       frame: noteFrame, outline: .note)
                items.append(.line(.init(points: [from, to], curved: false, paint: .noteStroke, dash: .dashed, start: .none, end: .none)))
                items.append(.shape(.init(outline: .note, frame: noteFrame, fill: .noteFill, stroke: .noteStroke, dash: .solid)))
                items.append(.text(.init(
                    lines: text.lines, position: CGPoint(x: noteFrame.midX, y: noteFrame.midY), alignment: .center, role: .label,
                    paint: .text, lineHeight: text.lineHeight, background: nil
                )))
            }
            items.append(.shape(.init(outline: node.outline, frame: frame, fill: node.fill, stroke: node.stroke, dash: node.dash)))
            switch node.content {
            case .empty:
                break
            case .label(let text):
                items.append(.text(.init(
                    lines: text.lines, position: CGPoint(x: frame.midX, y: frame.midY), alignment: .center, role: .label,
                    paint: .text, lineHeight: text.lineHeight, background: nil
                )))
            case .members(let header, let sections, let headerHeight, let sectionHeights):
                var y = frame.minY + 7
                for (offset, block) in header.enumerated() {
                    items.append(.text(.init(
                        lines: block.lines, position: CGPoint(x: frame.midX, y: y + block.size.height / 2), alignment: .center,
                        role: offset == header.count - 1 ? .strong : .edgeLabel, paint: offset == header.count - 1 ? .text : .mutedText,
                        lineHeight: block.lineHeight, background: nil
                    )))
                    y += block.size.height
                }
                y = frame.minY + headerHeight
                for (section, height) in zip(sections, sectionHeights) {
                    items.append(.line(.init(
                        points: [CGPoint(x: frame.minX, y: y), CGPoint(x: frame.maxX, y: y)], curved: false, paint: .nodeStroke,
                        dash: .solid, start: .none, end: .none
                    )))
                    var lineY = y + 5
                    for block in section {
                        items.append(.text(.init(
                            lines: block.lines, position: CGPoint(x: frame.minX + 12, y: lineY + block.size.height / 2),
                            alignment: .leading, role: .label, paint: .text, lineHeight: block.lineHeight, background: nil
                        )))
                        lineY += block.size.height
                    }
                    y += height
                }
            }
        }
        items += labels

        return MermaidLayout(
            size: placed.size, items: items, nodes: placedNodes, edges: placedEdges, clusters: placedClusters,
            crossings: placed.crossings
        )
    }

    private static func noteSize(_ text: Measure.Block) -> CGSize {
        CGSize(width: text.size.width + 20, height: text.size.height + 14)
    }

    static func labelBox(_ size: CGSize) -> CGSize {
        CGSize(width: size.width + 2 * labelPadding.width, height: size.height + 2 * labelPadding.height)
    }

    /// A cardinality beside the end of a relation, a little along the line and off to its side.
    private static func endText(_ text: Measure.Block, at end: CGPoint, toward next: CGPoint) -> MermaidLayout.Item {
        let dx = next.x - end.x
        let dy = next.y - end.y
        let length = max((dx * dx + dy * dy).squareRoot(), 0.001)
        let along = CGPoint(x: dx / length, y: dy / length)
        let side = CGPoint(x: -along.y, y: along.x)
        let reach = 10 + text.size.height / 2
        let offset = 8 + text.size.width / 2
        let position = CGPoint(x: end.x + along.x * reach + side.x * offset, y: end.y + along.y * reach + side.y * offset)
        return .text(.init(lines: text.lines, position: position, alignment: .center, role: .edgeLabel, paint: .mutedText,
                           lineHeight: text.lineHeight, background: nil))
    }
}
