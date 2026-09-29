import AlondraShared
import SwiftUI

struct KindStyle {
    let label: String
    let symbol: String
    let tint: Color
}

extension DomainV2.InboxItem {
    /// The server's inbox kind picks the style; the row never guesses it from text.
    var style: KindStyle {
        switch kind {
        case "action_request": KindStyle(label: String(localized: "Your turn"), symbol: "hand.point.right.fill", tint: .pink)
        case "question": KindStyle(label: String(localized: "Question"), symbol: "questionmark.bubble.fill", tint: .orange)
        case "pull_request": KindStyle(label: String(localized: "Pull request"), symbol: "arrow.triangle.pull", tint: .green)
        case "document_review": KindStyle(label: String(localized: "Document"), symbol: "doc.text.fill", tint: .blue)
        case "delivery": KindStyle(label: String(localized: "Finished"), symbol: "checkmark.seal.fill", tint: .green)
        default: KindStyle(label: String(localized: "Notice"), symbol: "bell.fill", tint: .teal)
        }
    }
}

func shortName(_ name: String?) -> String {
    guard let name, !name.isEmpty else { return String(localized: "unnamed") }
    return name.split(separator: "/").last.map(String.init) ?? name
}

struct InboxRow: View {
    let item: DomainV2.InboxItem
    var agent: String?
    var date: Int64?
    var stackedOn: String?
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
                Text(verbatim: ([style.label, shortName(agent)] + [date.map(ago)].compactMap { $0 }).joined(separator: " · "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                if let stackedOn {
                    Label(stackedOn, systemImage: "square.stack.3d.up")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
    }
}

struct NodeRow: View {
    let node: Node
    var tasks: [DomainV2.Task] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 8) {
                StatusDot(color: node.dotColor)
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
            ForEach(tasks) { task in
                HStack(spacing: 7) {
                    TaskDot(task: task, size: 6)
                    Text(task.fleetRowTitle(nodeDisplayName: node.displayName))
                        .strikethrough(task.state == .cancelled)
                        .lineLimit(1)
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.leading, 19)
            }
        }
        .help(tooltip)
        .accessibilityElement(children: .combine)
        .accessibilityValue(node.fleetLabel)
    }
}

extension NodeRow {
    private var tooltip: String {
        let header = "\(node.displayName) · \(node.fleetLabel)"
        let summary = node.summary.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        guard !summary.isEmpty else { return header }
        return header + "\n" + (summary.count > 140 ? summary.prefix(139) + "…" : summary)
    }
}

struct StatusDot: View {
    let color: Color
    var size: CGFloat = 8

    var body: some View {
        Circle()
            .fill(color)
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

struct TaskGroupLabel: View {
    let group: TaskStateGroup

    var body: some View {
        HStack(spacing: 7) {
            StatusDot(color: group.kind.color)
            Text(group.title)
                .fontWeight(.semibold)
            Spacer(minLength: 4)
            Text(group.count, format: .number)
                .font(.caption)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
        .font(.callout)
    }
}

extension DomainV2.TaskState {
    var color: Color {
        switch self {
        case .notStarted: .secondary
        case .waitingDependency: .indigo
        case .running: .green
        case .needsYou: .orange
        case .inReview: .mint
        case .closing: .teal
        case .done, .cancelled: .gray
        }
    }
}

/// A task's state dot; an action the user has to carry out takes the colour of "your turn", like its card.
struct TaskDot: View {
    let task: DomainV2.Task
    var size: CGFloat = 8
    @Environment(\.yourTurnTasks) private var yourTurn

    var body: some View {
        StatusDot(color: task.state == .needsYou && yourTurn.contains(task.id) ? .pink : task.state.color, size: size)
    }
}

extension TaskStateGroup.Kind {
    var color: Color {
        switch self {
        case .defining: .purple
        case .task(let state): state.color
        case .archived: .gray
        }
    }
}

extension NodeOccupancy {
    var color: Color {
        switch self {
        case .busy: .green
        case .free: .gray
        case .waitingMachine: .yellow
        }
    }
}

extension Node {
    /// The orchestrator keeps its node state; every other node shows whether it is busy.
    var dotColor: Color { isOrchestrator ? (orchestratorRuntime?.activityState ?? state).color : occupancy.color }
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
