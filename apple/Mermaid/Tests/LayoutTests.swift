@testable import AlondraMermaid
import CoreGraphics
import Testing

@Suite struct LayoutTests {
    static let graphs = Corpus.valid.filter { !$0.hasPrefix("seq_") }
    static let sequences = Corpus.valid.filter { $0.hasPrefix("seq_") }

    private func layout(_ name: String) throws -> MermaidLayout {
        Mermaid.layout(try Corpus.diagram(name), typography: Corpus.typography)
    }

    private func direction(_ name: String) throws -> Direction {
        switch try Corpus.diagram(name) {
        case .flowchart(let chart): chart.direction
        case .state(let state): state.direction
        case .classDiagram(let diagram): diagram.direction
        case .sequence: .topToBottom
        }
    }

    @Test(arguments: graphs)
    func nodesDoNotOverlap(_ name: String) throws {
        let nodes = try layout(name).nodes
        for (index, node) in nodes.enumerated() {
            for other in nodes[(index + 1)...] {
                let overlap = node.frame.intersection(other.frame)
                #expect(overlap.isNull || overlap.width < 0.5 || overlap.height < 0.5, "\(name): \(node.id) overlaps \(other.id)")
            }
        }
    }

    @Test(arguments: graphs)
    func edgesFollowTheDirection(_ name: String) throws {
        let direction = try direction(name)
        for edge in try layout(name).edges where !edge.reversed && edge.from != edge.to {
            let start = edge.points[0]
            let end = edge.points[edge.points.count - 1]
            let advance = switch direction {
            case .topToBottom: end.y - start.y
            case .bottomToTop: start.y - end.y
            case .leftToRight: end.x - start.x
            case .rightToLeft: start.x - end.x
            }
            #expect(advance > 0, "\(name): \(edge.from) -> \(edge.to) runs against \(direction)")
        }
    }

    @Test(arguments: graphs)
    func edgesStartAndEndOnANodeBorder(_ name: String) throws {
        let placed = try layout(name)
        func onBorder(_ point: CGPoint) -> Bool {
            placed.nodes.contains { abs(Boundary.norm(point, frame: $0.frame, outline: $0.outline) - 1) < 0.02 }
        }
        for edge in placed.edges {
            #expect(onBorder(edge.points[0]), "\(name): \(edge.from) -> \(edge.to) starts off a border")
            #expect(onBorder(edge.points[edge.points.count - 1]), "\(name): \(edge.from) -> \(edge.to) ends off a border")
        }
    }

    @Test(arguments: graphs)
    func clusterBoxesHoldTheirNodesAndKeepApart(_ name: String) throws {
        let placed = try layout(name)
        let frames = Dictionary(uniqueKeysWithValues: placed.nodes.map { ($0.id, $0.frame) })
        for cluster in placed.clusters {
            for member in cluster.members {
                #expect(cluster.frame.contains(frames[member]!), "\(name): \(member) outside \(cluster.id)")
            }
            for node in placed.nodes where !cluster.members.contains(node.id) {
                #expect(!cluster.frame.insetBy(dx: 0.5, dy: 0.5).intersects(node.frame), "\(name): \(node.id) inside \(cluster.id)")
            }
            for other in placed.clusters where other.id != cluster.id {
                let nested = Set(other.members).isSubset(of: cluster.members) || Set(cluster.members).isSubset(of: other.members)
                if !nested {
                    #expect(!cluster.frame.intersects(other.frame), "\(name): \(cluster.id) overlaps \(other.id)")
                }
            }
        }
    }

    @Test(arguments: Corpus.valid)
    func layoutIsTheSameEveryTime(_ name: String) throws {
        #expect(try layout(name) == layout(name))
    }

    @Test(arguments: graphs)
    func crossingsStayWithinTheAcceptedCount(_ name: String) throws {
        let crossings = try layout(name).crossings
        Corpus.record("\(crossings)\n", as: "\(name).crossings")
        let accepted = try #require(Corpus.read("valid", name, "crossings").flatMap { Int($0.trimmingCharacters(in: .whitespacesAndNewlines)) })
        #expect(crossings <= accepted)
    }

    @Test func crossingReductionUntanglesACrossedPair() throws {
        let source = """
        flowchart TD
            b1
            b2
            a1 --> b2
            a2 --> b1
            b1 --> c1
            b2 --> c2
        """
        #expect(Mermaid.layout(try Mermaid.parse(source), typography: Corpus.typography).crossings == 0)
    }

    @Test(arguments: sequences)
    func lanesFollowDeclarationAndRowsGrow(_ name: String) throws {
        let placed = try layout(name)
        #expect(zip(placed.lanes, placed.lanes.dropFirst()).allSatisfy { $0 < $1 }, "\(name): lanes out of order")
        #expect(zip(placed.messages, placed.messages.dropFirst()).allSatisfy { $0.y < $1.y }, "\(name): rows do not grow")
        guard case .sequence(let sequence) = try Corpus.diagram(name) else { return }
        #expect(placed.messages.map(\.from) == sequence.messages.map(\.from))
        #expect(placed.messages.map(\.to) == sequence.messages.map(\.to))
    }

    @Test(arguments: sequences)
    func blocksEncloseTheirMessages(_ name: String) throws {
        let placed = try layout(name)
        guard case .sequence(let sequence) = try Corpus.diagram(name) else { return }
        let lane = Dictionary(uniqueKeysWithValues: sequence.participants.enumerated().map { ($1.id, placed.lanes[$0]) })
        for block in placed.blocks {
            for index in block.messages {
                let message = placed.messages[index]
                #expect(block.frame.minY < message.y && message.y < block.frame.maxY, "\(name): row outside its block")
                #expect(block.frame.minX < lane[message.from]! && lane[message.to]! < block.frame.maxX, "\(name): lane outside its block")
            }
        }
    }

    @Test func blocksHoldExactlyTheMessagesWrittenInside() throws {
        let placed = try layout("seq_blocks")
        #expect(placed.blocks.map(\.messages) == [[0], [1, 2], [3], [4, 5]])
    }
}
