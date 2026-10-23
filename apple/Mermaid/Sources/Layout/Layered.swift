import CoreGraphics

/// The graph flowcharts, state and class diagrams are placed as: boxes of known size, edges
/// between them and nested clusters.
struct LayeredGraph {
    struct Node {
        var size: CGSize
        var cluster: Int?
        /// A box kept beside the node in its row, such as a note: before it and after it.
        var leading: CGSize? = nil
        var trailing: CGSize? = nil
    }

    struct Edge {
        var from: Int
        var to: Int
        var label: CGSize?
    }

    struct Cluster {
        var parent: Int?
        var title: CGSize
    }

    var direction: Direction
    var nodes: [Node]
    var edges: [Edge]
    var clusters: [Cluster]
}

struct LayeredResult {
    struct Route {
        /// Centre of `from`, the bends, centre of `to`; a self-loop starts and ends on the border.
        var points: [CGPoint]
        var reversed: Bool
        var selfLoop: Bool
        var labelCenter: CGPoint?
    }

    var nodes: [CGRect]
    var leading: [CGRect?]
    var trailing: [CGRect?]
    var routes: [Route]
    var clusters: [CGRect]
    var size: CGSize
    var crossings: Int
}

/// Sugiyama-style placement, the steps dagre and Graphviz follow: break cycles, rank by longest
/// path and tighten, add a dummy vertex per row an edge skips, order rows by barycentre sweeps and
/// transpositions, place each row by the median of its neighbours, keep clusters contiguous and
/// apart, and turn the result for the diagram's direction. Every edge spans two ranks so its label
/// has a row of its own between the rows of nodes.
struct Layered {
    static let nodeGap: CGFloat = 28
    static let dummyGap: CGFloat = 12
    static let clusterPad: CGFloat = 14
    static let clusterGap: CGFloat = 12
    static let margin: CGFloat = 8
    static let selfLoopReach: CGFloat = 22
    static let titleGap: CGFloat = 6
    static let attachmentGap: CGFloat = 14
    static let rankGap: CGFloat = 20

    private struct Vertex {
        var breadth: CGFloat
        var depth: CGFloat
        var rank: Int
        var cluster: Int?
        var isDummy: Bool
        var leadingSpace: CGFloat = 0
        var trailingSpace: CGFloat = 0
    }

    private let graph: LayeredGraph
    private var vertices: [Vertex] = []
    private var up: [[Int]] = []
    private var down: [[Int]] = []
    private var chains: [[Int]] = []
    private var reversed: [Bool] = []
    private var labelVertex: [Int?] = []
    private var layers: [[Int]] = []
    private var position: [CGFloat] = []
    private var paths: [[Int]] = []
    private var clusterPaths: [[Int]] = []
    private var clusterMembers: [[Int]] = []

    init(_ graph: LayeredGraph) {
        self.graph = graph
    }

    static func place(_ graph: LayeredGraph) -> LayeredResult {
        var layered = Layered(graph)
        return layered.run()
    }

    private var vertical: Bool { graph.direction.isVertical }

    private mutating func run() -> LayeredResult {
        clusterPaths = graph.clusters.indices.map { index in
            var path: [Int] = []
            var current: Int? = index
            while let cluster = current {
                path.insert(cluster, at: 0)
                current = graph.clusters[cluster].parent
            }
            return path
        }
        let ranks = rank()
        buildVertices(ranks: ranks)
        order()
        placeBreadth()
        pack()
        return result()
    }

    // MARK: Ranks

    private mutating func rank() -> [Int] {
        let count = graph.nodes.count
        reversed = Array(repeating: false, count: graph.edges.count)
        var outgoing = Array(repeating: [Int](), count: count)
        for (index, edge) in graph.edges.enumerated() where edge.from != edge.to {
            outgoing[edge.from].append(index)
        }
        var visited = Array(repeating: false, count: count)
        var onStack = Array(repeating: false, count: count)
        for root in 0..<count where !visited[root] {
            var stack: [(node: Int, next: Int)] = [(root, 0)]
            visited[root] = true
            onStack[root] = true
            while let top = stack.last {
                if top.next < outgoing[top.node].count {
                    stack[stack.count - 1].next += 1
                    let edge = outgoing[top.node][top.next]
                    let target = graph.edges[edge].to
                    if onStack[target] {
                        reversed[edge] = true
                    } else if !visited[target] {
                        visited[target] = true
                        onStack[target] = true
                        stack.append((target, 0))
                    }
                } else {
                    onStack[top.node] = false
                    stack.removeLast()
                }
            }
        }
        var successors = Array(repeating: [Int](), count: count)
        var predecessors = Array(repeating: [Int](), count: count)
        for (index, edge) in graph.edges.enumerated() where edge.from != edge.to {
            let (from, to) = reversed[index] ? (edge.to, edge.from) : (edge.from, edge.to)
            successors[from].append(to)
            predecessors[to].append(from)
        }
        var indegree = predecessors.map(\.count)
        var topological: [Int] = []
        var ready = (0..<count).filter { indegree[$0] == 0 }
        while !ready.isEmpty {
            let node = ready.removeFirst()
            topological.append(node)
            for next in successors[node] {
                indegree[next] -= 1
                if indegree[next] == 0 {
                    let insertAt = ready.firstIndex { $0 > next } ?? ready.count
                    ready.insert(next, at: insertAt)
                }
            }
        }
        var ranks = Array(repeating: 0, count: count)
        for node in topological {
            for previous in predecessors[node] {
                ranks[node] = max(ranks[node], ranks[previous] + 2)
            }
        }
        for node in topological.reversed() where !successors[node].isEmpty {
            ranks[node] = successors[node].map { ranks[$0] - 2 }.min()!
        }
        let lowest = ranks.min() ?? 0
        return ranks.map { $0 - lowest }
    }

    // MARK: Vertices

    private func commonCluster(_ a: Int?, _ b: Int?) -> Int? {
        guard let a, let b else { return nil }
        return zip(clusterPaths[a], clusterPaths[b]).prefix { $0 == $1 }.last?.0
    }

    private func breadthDepth(_ size: CGSize) -> (CGFloat, CGFloat) {
        vertical ? (size.width, size.height) : (size.height, size.width)
    }

    private mutating func buildVertices(ranks: [Int]) {
        vertices = graph.nodes.enumerated().map { index, node in
            let (breadth, depth) = breadthDepth(node.size)
            var vertex = Vertex(breadth: breadth, depth: depth, rank: ranks[index], cluster: node.cluster, isDummy: false)
            for (attachment, leading) in [(node.leading, true), (node.trailing, false)] {
                guard let attachment else { continue }
                let (along, across) = breadthDepth(attachment)
                vertex.depth = max(vertex.depth, across)
                if leading { vertex.leadingSpace = along + Self.attachmentGap } else { vertex.trailingSpace = along + Self.attachmentGap }
            }
            return vertex
        }
        up = Array(repeating: [], count: vertices.count)
        down = Array(repeating: [], count: vertices.count)
        chains = []
        labelVertex = []
        for (index, edge) in graph.edges.enumerated() {
            if edge.from == edge.to {
                let label = edge.label.map { vertical ? $0.width : $0.height } ?? 0
                vertices[edge.from].trailingSpace += Self.selfLoopReach + 4 + label
                chains.append([edge.from])
                labelVertex.append(nil)
                continue
            }
            let (from, to) = reversed[index] ? (edge.to, edge.from) : (edge.from, edge.to)
            let cluster = commonCluster(graph.nodes[from].cluster, graph.nodes[to].cluster)
            let start = ranks[from]
            let end = ranks[to]
            var middle = (start + end) / 2
            if middle % 2 == 0 { middle -= 1 }
            var chain = [from]
            var label: Int?
            for rank in (start + 1)..<end {
                var vertex = Vertex(breadth: 0, depth: 0, rank: rank, cluster: cluster, isDummy: true)
                if rank == middle, let size = edge.label {
                    (vertex.breadth, vertex.depth) = breadthDepth(size)
                    label = vertices.count
                }
                chain.append(vertices.count)
                vertices.append(vertex)
                up.append([])
                down.append([])
            }
            chain.append(to)
            for (a, b) in zip(chain, chain.dropFirst()) {
                down[a].append(b)
                up[b].append(a)
            }
            chains.append(chain)
            labelVertex.append(label)
        }
        paths = vertices.map { vertex in vertex.cluster.map { clusterPaths[$0] } ?? [] }
        clusterMembers = graph.clusters.indices.map { cluster in vertices.indices.filter { paths[$0].contains(cluster) } }
    }

    // MARK: Order

    private mutating func order() {
        let rankCount = (vertices.map(\.rank).max() ?? 0) + 1
        layers = Array(repeating: [], count: rankCount)
        for (index, vertex) in vertices.enumerated() {
            layers[vertex.rank].append(index)
        }
        for rank in layers.indices {
            let keys = Dictionary(uniqueKeysWithValues: layers[rank].enumerated().map { ($1, Double($0)) })
            layers[rank] = arrange(layers[rank], keys: keys, level: 0)
        }
        var best = layers
        var bestCrossings = totalCrossings()
        var stale = 0
        for iteration in 0..<24 where bestCrossings > 0 && stale < 4 {
            sweep(downward: iteration % 2 == 0)
            transpose()
            let crossings = totalCrossings()
            if crossings < bestCrossings {
                best = layers
                bestCrossings = crossings
                stale = 0
            } else {
                stale += 1
            }
        }
        layers = best
    }

    private func positions() -> [Int: Int] {
        var result: [Int: Int] = [:]
        for layer in layers {
            for (index, vertex) in layer.enumerated() {
                result[vertex] = index
            }
        }
        return result
    }

    private mutating func sweep(downward: Bool) {
        var index = positions()
        let ranks = downward ? Array(layers.indices.dropFirst()) : Array(layers.indices.dropLast().reversed())
        for rank in ranks {
            var keys: [Int: Double] = [:]
            for vertex in layers[rank] {
                let neighbours = downward ? up[vertex] : down[vertex]
                if neighbours.isEmpty {
                    keys[vertex] = Double(index[vertex]!)
                } else {
                    keys[vertex] = neighbours.map { Double(index[$0]!) }.reduce(0, +) / Double(neighbours.count)
                }
            }
            layers[rank] = arrange(layers[rank], keys: keys, level: 0)
            for (position, vertex) in layers[rank].enumerated() {
                index[vertex] = position
            }
        }
    }

    /// Sorts a row by `keys` while every cluster's vertices stay next to each other.
    private func arrange(_ row: [Int], keys: [Int: Double], level: Int) -> [Int] {
        var groups: [(key: Double, first: Int, members: [Int], cluster: Bool)] = []
        var groupOf: [Int: Int] = [:]
        for (offset, vertex) in row.enumerated() {
            if paths[vertex].count > level {
                let cluster = paths[vertex][level]
                if let group = groupOf[cluster] {
                    groups[group].members.append(vertex)
                    continue
                }
                groupOf[cluster] = groups.count
                groups.append((0, offset, [vertex], true))
            } else {
                groups.append((0, offset, [vertex], false))
            }
        }
        for index in groups.indices {
            let members = groups[index].members
            groups[index].key = members.map { keys[$0] ?? 0 }.reduce(0, +) / Double(members.count)
        }
        groups.sort { $0.key != $1.key ? $0.key < $1.key : $0.first < $1.first }
        return groups.flatMap { group in
            group.cluster ? arrange(group.members, keys: keys, level: level + 1) : group.members
        }
    }

    private mutating func transpose() {
        for _ in 0..<8 {
            var improved = false
            var index = positions()
            for rank in layers.indices {
                var row = layers[rank]
                guard row.count > 1 else { continue }
                for offset in 0..<(row.count - 1) {
                    let left = row[offset]
                    let right = row[offset + 1]
                    guard paths[left] == paths[right] else { continue }
                    if pairCrossings(left, right, index) > pairCrossings(right, left, index) {
                        row.swapAt(offset, offset + 1)
                        index[left] = offset + 1
                        index[right] = offset
                        improved = true
                    }
                }
                layers[rank] = row
            }
            if !improved { return }
        }
    }

    /// Crossings between the edges of `left` and `right` when `left` comes first in its row.
    private func pairCrossings(_ left: Int, _ right: Int, _ index: [Int: Int]) -> Int {
        var count = 0
        for neighbours in [(up[left], up[right]), (down[left], down[right])] {
            for a in neighbours.0 {
                for b in neighbours.1 where index[a]! > index[b]! {
                    count += 1
                }
            }
        }
        return count
    }

    private func totalCrossings() -> Int {
        let index = positions()
        return layers.indices.dropLast().reduce(0) { total, rank in
            let segments = layers[rank].flatMap { vertex in down[vertex].map { (index[vertex]!, index[$0]!) } }
            return total + Self.inversions(segments.map { (Double($0.0), Double($0.1)) })
        }
    }

    static func inversions(_ segments: [(Double, Double)]) -> Int {
        var count = 0
        for i in segments.indices {
            for j in segments.indices where j > i {
                let a = segments[i]
                let b = segments[j]
                if (a.0 - b.0) * (a.1 - b.1) < 0 { count += 1 }
            }
        }
        return count
    }

    // MARK: Breadth

    private func titleSpace(_ cluster: Int) -> CGFloat {
        vertical ? 0 : graph.clusters[cluster].title.height + Self.titleGap
    }

    /// Distance between the centres of two vertices that sit next to each other in a row.
    private func separation(_ left: Int, _ right: Int) -> CGFloat {
        let a = vertices[left]
        let b = vertices[right]
        let shared = zip(paths[left], paths[right]).prefix { $0 == $1 }.count
        let boundaries = CGFloat(paths[left].count - shared + paths[right].count - shared)
        let entering = paths[right].dropFirst(shared).reduce(CGFloat(0)) { $0 + titleSpace($1) }
        let bare = a.isDummy && b.isDummy && a.breadth == 0 && b.breadth == 0
        let gap = (bare ? Self.dummyGap : Self.nodeGap) + boundaries * Self.clusterPad + entering + a.trailingSpace + b.leadingSpace
        return (a.breadth + b.breadth) / 2 + gap
    }

    private mutating func placeBreadth() {
        position = Array(repeating: 0, count: vertices.count)
        for row in layers {
            var x: CGFloat = 0
            for (offset, vertex) in row.enumerated() {
                if offset > 0 { x += separation(row[offset - 1], vertex) }
                position[vertex] = x
            }
        }
        let above = up
        let below = down
        let both = zip(above, below).map { $0 + $1 }
        for pass in 0..<8 {
            let downward = pass % 2 == 0
            let ranks = downward ? Array(layers.indices) : Array(layers.indices.reversed())
            for rank in ranks {
                align(rank, neighbours: downward ? above : below)
            }
        }
        for rank in layers.indices {
            align(rank, neighbours: both)
        }
    }

    /// Moves a row's vertices as close as they can get to the median of their neighbours without
    /// changing their order or squeezing them closer than `separation` (pool adjacent violators).
    private mutating func align(_ rank: Int, neighbours: [[Int]]) {
        let row = layers[rank]
        guard !row.isEmpty else { return }
        var offsets: [CGFloat] = [0]
        for offset in 1..<row.count {
            offsets.append(offsets[offset - 1] + separation(row[offset - 1], row[offset]))
        }
        var blocks: [(value: CGFloat, weight: CGFloat, count: Int)] = []
        for (offset, vertex) in row.enumerated() {
            let around = neighbours[vertex].map { position[$0] }.sorted()
            let target: CGFloat
            let weight: CGFloat
            if around.isEmpty {
                target = position[vertex]
                weight = 0.05
            } else {
                let middle = around.count / 2
                target = around.count % 2 == 1 ? around[middle] : (around[middle - 1] + around[middle]) / 2
                weight = vertices[vertex].isDummy ? 2 : 1
            }
            blocks.append((target - offsets[offset], weight, 1))
            while blocks.count > 1, blocks[blocks.count - 2].value > blocks[blocks.count - 1].value {
                let last = blocks.removeLast()
                let previous = blocks.removeLast()
                let weight = previous.weight + last.weight
                blocks.append(((previous.value * previous.weight + last.value * last.weight) / weight, weight, previous.count + last.count))
            }
        }
        var offset = 0
        for block in blocks {
            for _ in 0..<block.count {
                position[row[offset]] = block.value + offsets[offset]
                offset += 1
            }
        }
    }

    // MARK: Clusters

    private enum Item: Hashable {
        case vertex(Int)
        case cluster(Int)
    }

    private func members(_ cluster: Int) -> [Int] {
        clusterMembers[cluster]
    }

    private func children(of cluster: Int?) -> [Item] {
        var items: [Item] = []
        for index in vertices.indices where vertices[index].cluster == cluster {
            items.append(.vertex(index))
        }
        for index in graph.clusters.indices where graph.clusters[index].parent == cluster && !members(index).isEmpty {
            items.append(.cluster(index))
        }
        return items
    }

    /// Breadth extent of a cluster's box, padding and title included.
    private func clusterBand(_ cluster: Int) -> (CGFloat, CGFloat) {
        var low = CGFloat.infinity
        var high = -CGFloat.infinity
        for vertex in members(cluster) {
            low = min(low, position[vertex] - vertices[vertex].breadth / 2 - vertices[vertex].leadingSpace)
            high = max(high, position[vertex] + vertices[vertex].breadth / 2 + vertices[vertex].trailingSpace)
        }
        let nested = graph.clusters.indices.filter { graph.clusters[$0].parent == cluster && !members($0).isEmpty }
        for child in nested {
            let band = clusterBand(child)
            low = min(low, band.0)
            high = max(high, band.1)
        }
        low -= Self.clusterPad + titleSpace(cluster)
        high += Self.clusterPad
        if vertical {
            high = max(high, low + graph.clusters[cluster].title.width + 2 * Self.clusterPad)
        }
        return (low, high)
    }

    private func band(_ item: Item) -> (CGFloat, CGFloat) {
        switch item {
        case .vertex(let vertex):
            (position[vertex] - vertices[vertex].breadth / 2 - vertices[vertex].leadingSpace,
             position[vertex] + vertices[vertex].breadth / 2 + vertices[vertex].trailingSpace)
        case .cluster(let cluster): clusterBand(cluster)
        }
    }

    private func ranks(_ item: Item) -> ClosedRange<Int> {
        switch item {
        case .vertex(let vertex):
            return vertices[vertex].rank...vertices[vertex].rank
        case .cluster(let cluster):
            let ranks = members(cluster).map { vertices[$0].rank }
            return ranks.min()!...ranks.max()!
        }
    }

    private func shiftable(_ item: Item) -> [Int] {
        switch item {
        case .vertex(let vertex): [vertex]
        case .cluster(let cluster): members(cluster)
        }
    }

    /// Clusters first, innermost out: no box overlaps a sibling box or a vertex outside it.
    private mutating func pack(_ cluster: Int? = nil) {
        let items = children(of: cluster)
        for case .cluster(let child) in items {
            pack(child)
        }
        guard items.count > 1 else { return }
        var itemOf: [Int: Item] = [:]
        for item in items {
            for vertex in shiftable(item) { itemOf[vertex] = item }
        }
        var before: [Item: Set<Item>] = [:]
        for row in layers {
            let sequence = row.compactMap { itemOf[$0] }
            for (left, right) in zip(sequence, sequence.dropFirst()) where left != right {
                before[right, default: []].insert(left)
            }
        }
        var remaining = items
        var placed: [Item] = []
        var done: Set<Item> = []
        var centre = Dictionary(uniqueKeysWithValues: items.map { item in
            let (low, high) = band(item)
            return (item, low + high)
        })
        while !remaining.isEmpty {
            let free = remaining.filter { (before[$0] ?? []).isSubset(of: done) }
            let pool = free.isEmpty ? remaining : free
            let next = pool.min { centre[$0]! < centre[$1]! }!
            remaining.removeAll { $0 == next }
            var shift: CGFloat = 0
            let span = ranks(next)
            let (low, _) = band(next)
            for earlier in placed where ranks(earlier).overlaps(span) {
                let gap: CGFloat
                if case .vertex(let a) = earlier, case .vertex(let b) = next {
                    gap = separation(a, b) - (vertices[a].breadth + vertices[b].breadth) / 2 - vertices[a].trailingSpace - vertices[b].leadingSpace
                } else {
                    gap = Self.clusterGap
                }
                shift = max(shift, band(earlier).1 + gap - low)
            }
            if shift > 0 {
                for vertex in shiftable(next) { position[vertex] += shift }
                centre[next]! += 2 * shift
            }
            placed.append(next)
            done.insert(next)
        }
    }

    // MARK: Result

    private func result() -> LayeredResult {
        var opening = Array(repeating: CGFloat(0), count: layers.count)
        var closing = Array(repeating: CGFloat(0), count: layers.count)
        for cluster in graph.clusters.indices where !members(cluster).isEmpty {
            let span = members(cluster).map { vertices[$0].rank }
            opening[span.min()!] += Self.clusterPad + (vertical ? graph.clusters[cluster].title.height + Self.titleGap : 0)
            closing[span.max()!] += Self.clusterPad
        }
        var depthOf: [CGFloat] = []
        var cursor: CGFloat = 0
        for (rank, row) in layers.enumerated() {
            let thickness = row.map { vertices[$0].depth }.max() ?? 0
            if rank > 0 {
                let previous = layers[rank - 1].map { vertices[$0].depth }.max() ?? 0
                cursor += previous / 2 + Self.rankGap + closing[rank - 1] + opening[rank] + thickness / 2
            }
            depthOf.append(cursor)
        }

        var clusterBoxes: [(low: CGFloat, high: CGFloat, near: CGFloat, far: CGFloat)?] = graph.clusters.indices.map { _ in nil }
        func box(_ cluster: Int) -> (low: CGFloat, high: CGFloat, near: CGFloat, far: CGFloat)? {
            if let existing = clusterBoxes[cluster] { return existing }
            let inside = members(cluster)
            guard !inside.isEmpty else { return nil }
            let (low, high) = clusterBand(cluster)
            var near = CGFloat.infinity
            var far = -CGFloat.infinity
            for vertex in inside {
                near = min(near, depthOf[vertices[vertex].rank] - vertices[vertex].depth / 2)
                far = max(far, depthOf[vertices[vertex].rank] + vertices[vertex].depth / 2)
            }
            for child in graph.clusters.indices where graph.clusters[child].parent == cluster {
                if let nested = box(child) {
                    near = min(near, nested.near)
                    far = max(far, nested.far)
                }
            }
            near -= Self.clusterPad
            far += Self.clusterPad
            if vertical {
                let title = graph.clusters[cluster].title.height + Self.titleGap
                if graph.direction == .bottomToTop { far += title } else { near -= title }
            }
            let result = (low, high, near, far)
            clusterBoxes[cluster] = result
            return result
        }
        let boxes = graph.clusters.indices.map { box($0) }

        var lowBreadth = CGFloat.infinity
        var highBreadth = -CGFloat.infinity
        var lowDepth = CGFloat.infinity
        var highDepth = -CGFloat.infinity
        for (index, vertex) in vertices.enumerated() {
            lowBreadth = min(lowBreadth, position[index] - vertex.breadth / 2 - vertex.leadingSpace)
            highBreadth = max(highBreadth, position[index] + vertex.breadth / 2 + vertex.trailingSpace)
            lowDepth = min(lowDepth, depthOf[vertex.rank] - vertex.depth / 2)
            highDepth = max(highDepth, depthOf[vertex.rank] + vertex.depth / 2)
        }
        for case let box? in boxes {
            lowBreadth = min(lowBreadth, box.low)
            highBreadth = max(highBreadth, box.high)
            lowDepth = min(lowDepth, box.near)
            highDepth = max(highDepth, box.far)
        }
        if vertices.isEmpty {
            lowBreadth = 0
            highBreadth = 0
            lowDepth = 0
            highDepth = 0
        }
        let breadthSpan = highBreadth - lowBreadth
        let depthSpan = highDepth - lowDepth
        let margin = Self.margin

        func point(breadth: CGFloat, depth: CGFloat) -> CGPoint {
            let b = breadth - lowBreadth + margin
            let d = depth - lowDepth + margin
            switch graph.direction {
            case .topToBottom: return CGPoint(x: b, y: d)
            case .bottomToTop: return CGPoint(x: b, y: depthSpan + 2 * margin - d)
            case .leftToRight: return CGPoint(x: d, y: b)
            case .rightToLeft: return CGPoint(x: depthSpan + 2 * margin - d, y: b)
            }
        }
        func center(_ vertex: Int) -> CGPoint {
            point(breadth: position[vertex], depth: depthOf[vertices[vertex].rank])
        }

        let nodeFrames = graph.nodes.indices.map { index -> CGRect in
            let size = graph.nodes[index].size
            let middle = center(index)
            return CGRect(x: middle.x - size.width / 2, y: middle.y - size.height / 2, width: size.width, height: size.height)
        }
        func attachment(_ index: Int, leading: Bool) -> CGRect? {
            guard let size = leading ? graph.nodes[index].leading : graph.nodes[index].trailing else { return nil }
            let frame = nodeFrames[index]
            let gap = Self.attachmentGap
            if vertical {
                let x = leading ? frame.minX - gap - size.width : frame.maxX + gap
                return CGRect(x: x, y: frame.midY - size.height / 2, width: size.width, height: size.height)
            }
            let y = leading ? frame.minY - gap - size.height : frame.maxY + gap
            return CGRect(x: frame.midX - size.width / 2, y: y, width: size.width, height: size.height)
        }
        var routes: [LayeredResult.Route] = []
        for (index, edge) in graph.edges.enumerated() {
            if edge.from == edge.to {
                routes.append(selfLoop(nodeFrames[edge.from], label: edge.label))
                continue
            }
            var points = chains[index].map(center)
            if reversed[index] { points.reverse() }
            routes.append(.init(
                points: points, reversed: reversed[index], selfLoop: false, labelCenter: labelVertex[index].map(center)
            ))
        }
        let clusterFrames = boxes.map { box -> CGRect in
            guard let box else { return .null }
            let a = point(breadth: box.low, depth: box.near)
            let b = point(breadth: box.high, depth: box.far)
            return CGRect(x: min(a.x, b.x), y: min(a.y, b.y), width: abs(a.x - b.x), height: abs(a.y - b.y))
        }
        let size = vertical
            ? CGSize(width: breadthSpan + 2 * margin, height: depthSpan + 2 * margin)
            : CGSize(width: depthSpan + 2 * margin, height: breadthSpan + 2 * margin)
        return LayeredResult(
            nodes: nodeFrames, leading: graph.nodes.indices.map { attachment($0, leading: true) },
            trailing: graph.nodes.indices.map { attachment($0, leading: false) }, routes: routes, clusters: clusterFrames, size: size, crossings: finalCrossings()
        )
    }

    private func finalCrossings() -> Int {
        layers.indices.dropLast().reduce(0) { total, rank in
            let segments = layers[rank].flatMap { vertex in
                down[vertex].map { (Double(position[vertex]), Double(position[$0])) }
            }
            return total + Self.inversions(segments)
        }
    }

    private func selfLoop(_ frame: CGRect, label: CGSize?) -> LayeredResult.Route {
        let reach = Self.selfLoopReach
        if vertical {
            let quarter = frame.height / 4
            let points = [
                CGPoint(x: frame.maxX, y: frame.midY - quarter),
                CGPoint(x: frame.maxX + reach, y: frame.midY - quarter - 4),
                CGPoint(x: frame.maxX + reach, y: frame.midY + quarter + 4),
                CGPoint(x: frame.maxX, y: frame.midY + quarter),
            ]
            let labelCenter = label.map { CGPoint(x: frame.maxX + reach + 4 + $0.width / 2, y: frame.midY) }
            return .init(points: points, reversed: false, selfLoop: true, labelCenter: labelCenter)
        }
        let quarter = frame.width / 4
        let points = [
            CGPoint(x: frame.midX - quarter, y: frame.maxY),
            CGPoint(x: frame.midX - quarter - 4, y: frame.maxY + reach),
            CGPoint(x: frame.midX + quarter + 4, y: frame.maxY + reach),
            CGPoint(x: frame.midX + quarter, y: frame.maxY),
        ]
        let labelCenter = label.map { CGPoint(x: frame.midX, y: frame.maxY + reach + 4 + $0.height / 2) }
        return .init(points: points, reversed: false, selfLoop: true, labelCenter: labelCenter)
    }
}
