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
        case .question(let q):
            switch q.origin {
            case "plan": KindStyle(label: "Plan", symbol: "list.bullet.clipboard.fill", tint: .purple)
            case "permission": KindStyle(label: "Permiso", symbol: "lock.shield.fill", tint: .red)
            default: KindStyle(label: "Pregunta", symbol: "questionmark.bubble.fill", tint: .orange)
            }
        case .report(let r):
            switch r.kind {
            case "document": KindStyle(label: "Documento", symbol: "doc.text.fill", tint: .blue)
            case "notice": KindStyle(label: "Aviso", symbol: "bell.fill", tint: .teal)
            case "final": KindStyle(label: "Terminado", symbol: "checkmark.seal.fill", tint: .green)
            default: KindStyle(label: "Progreso", symbol: "arrow.triangle.2.circlepath", tint: .gray)
            }
        }
    }
}

func shortName(_ name: String?) -> String {
    guard let name, !name.isEmpty else { return "sin nombre" }
    return name.split(separator: "/").last.map(String.init) ?? name
}

struct InboxRow: View {
    let item: InboxItem

    var body: some View {
        let style = item.style
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: style.symbol)
                .foregroundStyle(style.tint)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.title)
                    .fontWeight(.medium)
                    .lineLimit(2)
                Text("\(style.label) · \(shortName(item.nodeName)) · \(ago(item.date))")
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
            StatusDot(state: node.state, alive: node.alive)
            Text(shortName(node.displayName))
                .lineLimit(1)
                .foregroundStyle(node.alive ? .primary : .secondary)
            if node.isOrchestrator {
                Image(systemName: "sparkle")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .help("Orquestador")
            }
            if !node.alive {
                Image(systemName: "moon.zzz")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .help("Dormido")
            }
        }
        .help(tooltip)
        .accessibilityElement(children: .combine)
        .accessibilityValue(node.alive ? node.state.label : "\(node.state.label), dormido")
    }
}

extension NodeRow {
    private var tooltip: String {
        let header = "\(node.displayName) · \(node.state.label)"
        let summary = node.summary.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        guard !summary.isEmpty else { return header }
        return header + "\n" + (summary.count > 140 ? summary.prefix(139) + "…" : summary)
    }
}

struct StatusDot: View {
    let state: NodeState
    let alive: Bool
    var size: CGFloat = 8

    var body: some View {
        Circle()
            .fill(alive ? state.color : .clear)
            .strokeBorder(state.color.opacity(alive ? 0 : 0.7), lineWidth: 1.2)
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
            let urgent = nodes.filter { $0.alive && ($0.state == .waitingAnswer || $0.state == .unclear) }.count
            if urgent > 0 {
                Circle().fill(NodeState.waitingAnswer.color).frame(width: 6, height: 6)
                    .help(urgent == 1 ? "1 nodo espera" : "\(urgent) nodos esperan")
            }
        }
    }
}

extension NodeState {
    var color: Color {
        switch self {
        case .waitingAnswer: .orange
        case .unclear: .purple
        case .delivered: .blue
        case .waitingMachine: .yellow
        case .working: .green
        case .done: .gray
        }
    }
}
