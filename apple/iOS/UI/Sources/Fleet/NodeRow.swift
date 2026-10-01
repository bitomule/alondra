import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One node in the Fleet list: its state, what it last said, the tasks under it, and how many
/// things wait for you on it.
struct NodeRow: View {
    let node: FleetOverview.Node
    let open: (AlondraContract.Link) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { if let link = node.link { open(link) } } label: {
                HStack(alignment: .top, spacing: 12) {
                    Glyph(symbol: node.isOrchestrator ? "point.3.connected.trianglepath.dotted" : "desktopcomputer",
                          tint: tint, size: .small)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(node.title).font(.mono(.subheadline, weight: .semibold)).foregroundStyle(Palette.ink)
                            .lineLimit(1).truncationMode(.middle)
                        if let state = node.state {
                            Text(state).font(.caption.weight(.semibold)).foregroundStyle(tint).lineLimit(2)
                        }
                        if !node.badges.isEmpty {
                            Text(node.badges.map(\.text).joined(separator: " · "))
                                .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        }
                    }
                    Spacer(minLength: 0)
                    if node.pending > 0 {
                        Text(node.pending, format: .number)
                            .font(.caption.weight(.bold).monospacedDigit())
                            .foregroundStyle(Palette.onAccent)
                            .frame(minWidth: 22, minHeight: 22)
                            .background(Palette.dawn, in: .capsule)
                    }
                }
                .padding(.horizontal, 14).padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            ForEach(node.tasks, id: \.id) { task in
                Button { if let link = task.link { open(link) } } label: {
                    HStack(spacing: 8) {
                        Glyph(symbol: task.struck ? "checkmark" : "circle.dashed", tint: Palette.slate, size: .tiny)
                        Text(task.title).font(.subheadline.weight(.medium))
                            .foregroundStyle(task.struck ? .secondary : Palette.ink)
                            .strikethrough(task.struck).lineLimit(2).multilineTextAlignment(.leading)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 14).padding(.bottom, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
            if let last = node.lastMessage {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(last.title).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                    Spacer(minLength: 0)
                    if case .time(let at, let format)? = last.trailing {
                        TimeText(raw: at, format: format).font(.caption2).foregroundStyle(.tertiary)
                    }
                }
                .padding(.horizontal, 14).padding(.bottom, 12)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .accessibilityElement(children: .contain)
    }

    private var tint: Color {
        if node.isOrchestrator { return Palette.ember }
        return switch node.badges.first?.tone {
        case .success: Palette.sky
        case .critical: Palette.rose
        default: Palette.sage
        }
    }
}
