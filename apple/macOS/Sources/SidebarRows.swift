import AlondraShared
import SwiftUI

struct KindStyle {
    let label: String
    let symbol: String
    let tint: Color
}

extension InboxItem {
    var style: KindStyle {
        switch self {
        case .question(let q) where q.isAction:
            KindStyle(label: String(localized: "Your turn"), symbol: "hand.point.right.fill", tint: .pink)
        case .question(let q):
            switch q.origin {
            case "plan": KindStyle(label: String(localized: "Plan"), symbol: "list.bullet.clipboard.fill", tint: .purple)
            case "permission": KindStyle(label: String(localized: "Permission"), symbol: "lock.shield.fill", tint: .red)
            default: KindStyle(label: String(localized: "Question"), symbol: "questionmark.bubble.fill", tint: .orange)
            }
        case .pullRequest:
            KindStyle(label: String(localized: "Pull request"), symbol: "arrow.triangle.pull", tint: .green)
        case .report(let r):
            switch r.kind {
            case "document": KindStyle(label: String(localized: "Document"), symbol: "doc.text.fill", tint: .blue)
            case "notice": KindStyle(label: String(localized: "Notice"), symbol: "bell.fill", tint: .teal)
            case "final": KindStyle(label: String(localized: "Finished"), symbol: "checkmark.seal.fill", tint: .green)
            default: KindStyle(label: String(localized: "Progress"), symbol: "arrow.triangle.2.circlepath", tint: .gray)
            }
        }
    }
}

func shortName(_ name: String?) -> String {
    guard let name, !name.isEmpty else { return String(localized: "unnamed") }
    return name.split(separator: "/").last.map(String.init) ?? name
}

struct InboxRow: View {
    let item: InboxItem
    var asks = false

    var body: some View {
        let style = item.style
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: style.symbol)
                .foregroundStyle(style.tint)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(item.title)
                        .fontWeight(.medium)
                        .lineLimit(2)
                    if asks {
                        Image(systemName: "questionmark.bubble.fill")
                            .font(.caption)
                            .foregroundStyle(.orange)
                            .help("The node asks something")
                            .accessibilityLabel("The node asks something")
                    }
                }
                Text(verbatim: "\(style.label) · \(shortName(item.nodeName)) · \(ago(item.date))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
    }
}

struct NodeRow: View {
    let node: Node

    var body: some View {
        HStack(spacing: 8) {
            StatusDot(state: node.state)
            Text(shortName(node.displayName))
                .lineLimit(1)
            if node.isOrchestrator {
                Image(systemName: "sparkle")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .help("Orchestrator")
            }
            if let badge = node.backgroundBadge {
                Spacer(minLength: 4)
                Text(verbatim: badge.text)
                    .font(.caption)
                    .foregroundStyle(badge.suspect ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                    .lineLimit(1)
            }
        }
        .help(tooltip)
        .accessibilityElement(children: .combine)
        .accessibilityValue(node.stateLabel)
    }
}

extension NodeRow {
    private var tooltip: String {
        let header = "\(node.displayName) · \(node.stateLabel)"
        let summary = node.summary.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        guard !summary.isEmpty else { return header }
        return header + "\n" + (summary.count > 140 ? summary.prefix(139) + "…" : summary)
    }
}

struct StatusDot: View {
    let state: NodeState
    var size: CGFloat = 8

    var body: some View {
        Circle()
            .fill(state.color)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

struct ProjectHeader: View {
    let name: String
    let nodes: [Node]

    var body: some View {
        HStack(spacing: 6) {
            Text(name)
            let urgent = nodes.filter { $0.state == .waitingAnswer || $0.state == .yourTurn || $0.state == .waitingMerge || $0.state == .unclear }.count
            if urgent > 0 {
                Circle().fill(NodeState.waitingAnswer.color).frame(width: 6, height: 6)
                    .help("\(urgent) nodes waiting")
            }
        }
    }
}

extension NodeState {
    var color: Color {
        switch self {
        case .waitingAnswer: .orange
        case .yourTurn: .pink
        case .waitingMerge: .mint
        case .unclear: .purple
        case .stalled: .red
        case .delivered: .blue
        case .waitingMachine: .yellow
        case .working: .green
        case .done: .gray
        }
    }
}
