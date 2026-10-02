import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One Fleet card: a node and its one task. The state leads, then the task's title, the node's
/// name when it adds something, and what it does now or said last.
struct NodeRow: View {
    let node: FleetOverview.Node
    let open: (AlondraContract.Link) -> Void

    var body: some View {
        Button { if let link = node.link { open(link) } } label: {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Glyph(symbol: node.isOrchestrator ? "point.3.connected.trianglepath.dotted" : NodeStyle.symbol(tone: node.state?.tone),
                          tint: tint, size: .small, pulse: !node.isOrchestrator && node.state?.tone == .info)
                    if let state = node.state {
                        Text(state.text).font(.caption.weight(.semibold)).foregroundStyle(tint).lineLimit(1)
                    }
                    Spacer(minLength: 8)
                    if node.pending > 0 { WaitingBadge(count: Int(node.pending)) }
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(node.title).font(.body.weight(.semibold)).foregroundStyle(Palette.ink)
                        .lineLimit(3).multilineTextAlignment(.leading)
                    if let subtitle = node.subtitle {
                        Text(subtitle).font(.mono(.caption)).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    }
                }
                activity
                badges
            }
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var activity: some View {
        if node.activity != nil || node.activityAt != nil {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if let text = node.activity {
                    Text(text).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer(minLength: 4)
                if let at = node.activityAt {
                    TimeText(raw: at).font(.caption).foregroundStyle(.tertiary).fixedSize()
                }
            }
        }
    }

    @ViewBuilder private var badges: some View {
        if !node.badges.isEmpty {
            HStack(spacing: 6) {
                ForEach(Array(node.badges.enumerated()), id: \.offset) { _, badge in
                    let tint = badge.tone == .warning ? Palette.ember : Palette.slate
                    HStack(spacing: 4) {
                        Image(systemName: badge.tone == .warning ? "exclamationmark.triangle.fill" : "square.stack.3d.up")
                            .accessibilityHidden(true)
                        Text(badge.text)
                    }
                    .font(.caption.weight(.semibold)).foregroundStyle(tint)
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .background(tint.opacity(0.12), in: .capsule)
                    .lineLimit(1)
                }
            }
            .padding(.top, 2)
        }
    }

    private var tint: Color {
        node.isOrchestrator ? Palette.ember : NodeStyle.tint(tone: node.state?.tone)
    }
}
