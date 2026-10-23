import CoreGraphics

/// Participants in columns, in the order they are declared; messages, notes and block edges in
/// rows that only grow downwards; `loop`/`alt`/`opt`/`par` boxes around the rows they hold.
struct SequenceLayout {
    static let margin: CGFloat = 8
    static let columnGap: CGFloat = 24
    static let rowGap: CGFloat = 14
    static let selfReach: CGFloat = 26
    static let blockInset: CGFloat = 10
    static let activationWidth: CGFloat = 10

    private let diagram: SequenceDiagram
    private let measure: Measure
    private let column: [String: Int]
    private var items: [MermaidLayout.Item] = []
    private var placedMessages: [MermaidLayout.PlacedMessage] = []
    private var placedBlocks: [MermaidLayout.PlacedBlock] = []
    private var lanes: [CGFloat] = []
    private var headers: [(block: Measure.Block, size: CGSize)] = []
    private var active: [String: Int] = [:]
    private var activations: [(participant: String, level: Int, top: CGFloat)] = []
    private var activationBars: [MermaidLayout.Item] = []
    private var number = 0
    private var y: CGFloat = 0
    private var rightEdge: CGFloat = 0

    init(_ diagram: SequenceDiagram, measure: Measure) {
        self.diagram = diagram
        self.measure = measure
        column = Dictionary(uniqueKeysWithValues: diagram.participants.enumerated().map { ($1.id, $0) })
    }

    func layout() -> MermaidLayout {
        var layout = self
        return layout.run()
    }

    private mutating func run() -> MermaidLayout {
        headers = diagram.participants.map { participant in
            let text = measure.text(participant.label, .label)
            switch participant.kind {
            case .participant: return (text, CGSize(width: max(text.size.width + 28, 64), height: text.size.height + 18))
            case .actor: return (text, CGSize(width: max(text.size.width + 12, 48), height: 40 + text.size.height + 6))
            }
        }
        placeLanes()
        let headerHeight = headers.map(\.size.height).max() ?? 0
        y = Self.margin + headerHeight + Self.rowGap
        place(diagram.items, depth: 0)
        for open in activations {
            activationBars.append(bar(open.participant, level: open.level, from: open.top, to: y))
        }
        let bottom = y + Self.rowGap / 2
        func isBlockOutline(_ item: MermaidLayout.Item) -> Bool {
            if case .shape(let shape) = item { shape.fill == .clear } else { false }
        }
        var result: [MermaidLayout.Item] = lanes.map { lane in
            .line(.init(
                points: [CGPoint(x: lane, y: Self.margin + headerHeight), CGPoint(x: lane, y: bottom)], curved: false,
                paint: .nodeStroke, dash: .dashed, start: .none, end: .none
            ))
        }
        result += items.filter(isBlockOutline)
        result += activationBars
        result += items.filter { !isBlockOutline($0) }
        for (index, participant) in diagram.participants.enumerated() {
            result += header(index, participant: participant, top: Self.margin, height: headerHeight)
            result += header(index, participant: participant, top: bottom, height: headerHeight)
        }
        return MermaidLayout(
            size: CGSize(width: rightEdge + Self.margin, height: bottom + headerHeight + Self.margin), items: result,
            lanes: lanes, messages: placedMessages, blocks: placedBlocks
        )
    }

    // MARK: Columns

    private mutating func placeLanes() {
        let count = diagram.participants.count
        guard count > 0 else { return }
        var needs: [(Int, Int, CGFloat)] = []
        var leftNeed: CGFloat = headers[0].size.width / 2
        var rightNeed: CGFloat = headers[count - 1].size.width / 2
        func visit(_ items: [SequenceDiagram.Item]) {
            for item in items {
                switch item {
                case .message(let message):
                    guard let from = column[message.from], let to = column[message.to] else { continue }
                    let width = measure.text(message.text, .edgeLabel).size.width + (diagram.autonumber ? 44 : 24)
                    if from == to {
                        let reach = Self.selfReach + width
                        if from == count - 1 { rightNeed = max(rightNeed, reach) } else { needs.append((from, from + 1, reach)) }
                    } else {
                        needs.append((min(from, to), max(from, to), width))
                    }
                case .note(let note):
                    let width = measure.text(note.text, .label).size.width + 24
                    let ids = note.participants.compactMap { column[$0] }
                    guard let first = ids.min(), let last = ids.max() else { continue }
                    switch note.placement {
                    case .leftOf:
                        if first == 0 { leftNeed = max(leftNeed, width + 12) } else { needs.append((first - 1, first, width + 12)) }
                    case .rightOf:
                        if first == count - 1 { rightNeed = max(rightNeed, width + 12) } else { needs.append((first, first + 1, width + 12)) }
                    case .over:
                        if first == last {
                            leftNeed = first == 0 ? max(leftNeed, width / 2) : leftNeed
                            rightNeed = first == count - 1 ? max(rightNeed, width / 2) : rightNeed
                        } else {
                            needs.append((first, last, width - 40))
                        }
                    }
                case .block(let block):
                    for section in block.sections { visit(section.items) }
                case .activate, .deactivate:
                    break
                }
            }
        }
        visit(diagram.items)
        var centres = [Self.margin + leftNeed + Self.blockInset]
        for index in 1..<max(count, 1) {
            centres.append(centres[index - 1] + headers[index - 1].size.width / 2 + Self.columnGap + headers[index].size.width / 2)
        }
        for (low, high, width) in needs.sorted(by: { $0.1 - $0.0 < $1.1 - $1.0 }) where centres[high] - centres[low] < width {
            let shift = width - (centres[high] - centres[low])
            for index in high..<count { centres[index] += shift }
        }
        lanes = centres
        rightEdge = centres[count - 1] + rightNeed + Self.blockInset
    }

    // MARK: Rows

    private mutating func place(_ entries: [SequenceDiagram.Item], depth: Int) {
        for entry in entries {
            switch entry {
            case .message(let message): placeMessage(message)
            case .note(let note): placeNote(note)
            case .activate(let id): activate(id)
            case .deactivate(let id): deactivate(id)
            case .block(let block): placeBlock(block, depth: depth)
            }
        }
    }

    private func edgeX(_ id: String, toward other: CGFloat) -> CGFloat {
        let lane = lanes[column[id]!]
        guard let level = active[id], level > 0 else { return lane }
        let offset = Self.activationWidth / 2 + CGFloat(level - 1) * 4
        return other >= lane ? lane + offset : lane - offset
    }

    private mutating func placeMessage(_ message: SequenceDiagram.Message) {
        let text = measure.text(message.text, .edgeLabel)
        number += 1
        let from = lanes[column[message.from]!]
        let to = lanes[column[message.to]!]
        let dash: MermaidLayout.Dash = message.stroke == .dotted ? .dashed : .solid
        let end: MermaidLayout.Marker = switch message.head {
        case .none: .none
        case .arrow: .arrow
        case .cross: .cross
        case .open: .openArrow
        }
        if message.from == message.to {
            y += text.size.height
            let top = y
            let start = edgeX(message.from, toward: from + 1)
            items.append(.line(.init(
                points: [CGPoint(x: start, y: top), CGPoint(x: from + Self.selfReach, y: top), CGPoint(x: from + Self.selfReach, y: top + 18),
                         CGPoint(x: start, y: top + 18)],
                curved: false, paint: .edge, dash: dash, start: .none, end: end
            )))
            if !text.lines.isEmpty {
                items.append(.text(.init(
                    lines: text.lines, position: CGPoint(x: from + Self.selfReach + 6, y: top - text.size.height / 2 - 1),
                    alignment: .leading, role: .edgeLabel, paint: .text, lineHeight: text.lineHeight, background: nil
                )))
            }
            badge(at: CGPoint(x: start, y: top))
            placedMessages.append(.init(from: message.from, to: message.to, y: top))
            y = top + 18 + Self.rowGap
        } else {
            y += max(text.size.height, 4) + 2
            if message.activation == .activateTarget { activate(message.to) }
            let startX = edgeX(message.from, toward: to)
            let endX = edgeX(message.to, toward: from)
            items.append(.line(.init(
                points: [CGPoint(x: startX, y: y), CGPoint(x: endX, y: y)], curved: false, paint: .edge, dash: dash, start: .none, end: end
            )))
            if !text.lines.isEmpty {
                items.append(.text(.init(
                    lines: text.lines, position: CGPoint(x: (from + to) / 2, y: y - text.size.height / 2 - 3), alignment: .center,
                    role: .edgeLabel, paint: .text, lineHeight: text.lineHeight, background: nil
                )))
            }
            badge(at: CGPoint(x: startX, y: y))
            placedMessages.append(.init(from: message.from, to: message.to, y: y))
            y += Self.rowGap
        }
        if message.activation == .deactivateSource { deactivate(message.from) }
    }

    private mutating func badge(at point: CGPoint) {
        guard diagram.autonumber else { return }
        let text = measure.text("\(number)", .edgeLabel)
        let diameter = max(text.size.width, text.size.height) + 4
        items.append(.shape(.init(
            outline: .circle, frame: CGRect(x: point.x - diameter / 2, y: point.y - diameter / 2, width: diameter, height: diameter),
            fill: .accent, stroke: .accent, dash: .solid
        )))
        items.append(.text(.init(lines: text.lines, position: point, alignment: .center, role: .edgeLabel, paint: .labelBackground,
                                 lineHeight: text.lineHeight, background: nil)))
    }

    private mutating func placeNote(_ note: SequenceDiagram.Note) {
        let text = measure.text(note.text, .label)
        let ids = note.participants.compactMap { column[$0] }
        guard let first = ids.min(), let last = ids.max() else { return }
        let width = text.size.width + 20
        let height = text.size.height + 12
        let frame: CGRect
        switch note.placement {
        case .leftOf: frame = CGRect(x: lanes[first] - 12 - width, y: y, width: width, height: height)
        case .rightOf: frame = CGRect(x: lanes[first] + 12, y: y, width: width, height: height)
        case .over:
            let low = lanes[first] - (first == last ? 0 : 20)
            let high = lanes[last] + (first == last ? 0 : 20)
            let span = max(high - low, width)
            frame = CGRect(x: (low + high) / 2 - span / 2, y: y, width: span, height: height)
        }
        items.append(.shape(.init(outline: .note, frame: frame, fill: .noteFill, stroke: .noteStroke, dash: .solid)))
        items.append(.text(.init(lines: text.lines, position: CGPoint(x: frame.midX, y: frame.midY), alignment: .center, role: .label,
                                 paint: .text, lineHeight: text.lineHeight, background: nil)))
        rightEdge = max(rightEdge, frame.maxX + Self.margin)
        y = frame.maxY + Self.rowGap
    }

    private mutating func activate(_ id: String) {
        let level = (active[id] ?? 0) + 1
        active[id] = level
        activations.append((id, level, y))
    }

    private mutating func deactivate(_ id: String) {
        guard let level = active[id], level > 0,
              let open = activations.lastIndex(where: { $0.participant == id && $0.level == level }) else { return }
        let entry = activations.remove(at: open)
        activationBars.append(bar(id, level: level, from: entry.top, to: y - Self.rowGap + 4))
        active[id] = level - 1
    }

    private func bar(_ id: String, level: Int, from top: CGFloat, to bottom: CGFloat) -> MermaidLayout.Item {
        let lane = lanes[column[id]!] + CGFloat(level - 1) * 4
        let height = max(bottom - top, 12)
        return .shape(.init(
            outline: .rectangle, frame: CGRect(x: lane - Self.activationWidth / 2, y: top - 4, width: Self.activationWidth, height: height),
            fill: .clusterFill, stroke: .nodeStroke, dash: .solid
        ))
    }

    private func participants(in entries: [SequenceDiagram.Item]) -> [Int] {
        entries.flatMap { entry -> [Int] in
            switch entry {
            case .message(let message): [column[message.from]!, column[message.to]!]
            case .note(let note): note.participants.compactMap { column[$0] }
            case .activate(let id), .deactivate(let id): [column[id]!]
            case .block(let block): block.sections.flatMap { participants(in: $0.items) }
            }
        }
    }

    private func reachesRight(_ entries: [SequenceDiagram.Item], lane: Int) -> Bool {
        entries.contains { entry in
            switch entry {
            case .message(let message): message.from == message.to && column[message.from] == lane
            case .block(let block): block.sections.contains { reachesRight($0.items, lane: lane) }
            default: false
            }
        }
    }

    private mutating func placeBlock(_ block: SequenceDiagram.Block, depth: Int) {
        let all = block.sections.flatMap(\.items)
        let touched = participants(in: all)
        let low = touched.min() ?? 0
        let high = touched.max() ?? max(lanes.count - 1, 0)
        let inset = Self.blockInset * CGFloat(max(3 - depth, 1))
        var left = lanes[low] - inset - 14
        var right = lanes[high] + inset + 14
        if reachesRight(all, lane: high) { right = max(right, lanes[high] + Self.selfReach + 14) }
        let keyword = measure.text(block.kind.rawValue, .title)
        let top = y
        let firstLabel = measure.text(block.sections.first.map { $0.label.isEmpty ? "" : "[\($0.label)]" } ?? "", .edgeLabel)
        left = min(left, lanes[low] - 30)
        right = max(right, left + keyword.size.width + firstLabel.size.width + 36)
        let tab = keyword.size.height + 6
        y += tab + 6
        let firstMessage = placedMessages.count
        var dividers: [(CGFloat, Measure.Block)] = []
        for (offset, section) in block.sections.enumerated() {
            if offset > 0 {
                let label = measure.text(section.label.isEmpty ? "" : "[\(section.label)]", .edgeLabel)
                dividers.append((y, label))
                y += label.size.height + 8
            }
            place(section.items, depth: depth + 1)
        }
        let frame = CGRect(x: left, y: top, width: right - left, height: y - top)
        y += Self.rowGap / 2
        items.append(.shape(.init(outline: .rectangle, frame: frame, fill: .clear, stroke: .clusterStroke, dash: .solid)))
        let tabFrame = CGRect(x: frame.minX, y: frame.minY, width: keyword.size.width + 14, height: tab)
        items.append(.shape(.init(outline: .rectangle, frame: tabFrame, fill: .clusterFill, stroke: .clusterStroke, dash: .solid)))
        items.append(.text(.init(lines: keyword.lines, position: CGPoint(x: tabFrame.midX, y: tabFrame.midY), alignment: .center,
                                 role: .title, paint: .mutedText, lineHeight: keyword.lineHeight, background: nil)))
        if !firstLabel.lines.isEmpty, firstLabel.lines != [""] {
            items.append(.text(.init(lines: firstLabel.lines, position: CGPoint(x: tabFrame.maxX + 8, y: tabFrame.midY),
                                     alignment: .leading, role: .edgeLabel, paint: .mutedText, lineHeight: firstLabel.lineHeight,
                                     background: nil)))
        }
        for (dividerY, label) in dividers {
            items.append(.line(.init(points: [CGPoint(x: frame.minX, y: dividerY), CGPoint(x: frame.maxX, y: dividerY)], curved: false,
                                     paint: .clusterStroke, dash: .dashed, start: .none, end: .none)))
            if !label.lines.isEmpty, label.lines != [""] {
                items.append(.text(.init(lines: label.lines, position: CGPoint(x: frame.midX, y: dividerY + 4 + label.size.height / 2),
                                         alignment: .center, role: .edgeLabel, paint: .mutedText, lineHeight: label.lineHeight,
                                         background: nil)))
            }
        }
        rightEdge = max(rightEdge, frame.maxX + Self.margin)
        placedBlocks.append(.init(frame: frame, messages: Array(firstMessage..<placedMessages.count)))
    }

    private func header(_ index: Int, participant: SequenceDiagram.Participant, top: CGFloat, height: CGFloat) -> [MermaidLayout.Item] {
        let (text, size) = headers[index]
        let lane = lanes[index]
        switch participant.kind {
        case .participant:
            let frame = CGRect(x: lane - size.width / 2, y: top + (height - size.height) / 2, width: size.width, height: size.height)
            return [
                .shape(.init(outline: .rounded, frame: frame, fill: .nodeFill, stroke: .nodeStroke, dash: .solid)),
                .text(.init(lines: text.lines, position: CGPoint(x: frame.midX, y: frame.midY), alignment: .center, role: .label,
                            paint: .text, lineHeight: text.lineHeight, background: nil)),
            ]
        case .actor:
            let figure = CGRect(x: lane - 12, y: top + 2, width: 24, height: 36)
            return [
                .actor(figure, .nodeStroke),
                .text(.init(lines: text.lines, position: CGPoint(x: lane, y: figure.maxY + 4 + text.size.height / 2), alignment: .center,
                            role: .label, paint: .text, lineHeight: text.lineHeight, background: nil)),
            ]
        }
    }
}
