import AlondraContract
import AlondraDesign
import SwiftUI

/// The pill shares the navigation bar with Settings. A toolbar item wider than its slot is folded,
/// with Settings, into a "…" menu, so the pill gives up numbers before it outgrows the slot: first
/// the RAM and CPU figures, then every figure, keeping each reading's bar.
struct MachineBadge: View {
    let gauges: [AlondraContract.Gauge]
    let width: CGFloat

    static let reservedBarWidth: CGFloat = 120

    var body: some View {
        ViewThatFits(in: .horizontal) {
            pill { _ in true }
            pill { !Self.machineLoad.contains($0.id) }
            pill { _ in false }
        }
        .modifier(ProposedWidth(width: max(width - Self.reservedBarWidth, 0)))
    }

    private static let machineLoad: Set<String> = ["memory", "cpu"]

    private func pill(showsText: @escaping (AlondraContract.Gauge) -> Bool) -> some View {
        HStack(spacing: 6) {
            Circle().fill(Palette.sage).frame(width: 8, height: 8)
                .shadow(color: Palette.sage.opacity(0.8), radius: 4)
            Image(systemName: "desktopcomputer").font(.footnote)
            ForEach(gauges.filter { $0.id != "disk" }, id: \.id) { gauge in
                HStack(spacing: 3) {
                    ReadingBar(value: gauge.value, tint: gauge.level.tint)
                    if showsText(gauge) {
                        Text(gauge.text).font(.caption2.monospacedDigit().weight(.medium))
                            .foregroundStyle(gauge.level == .hot ? Palette.rose : Palette.ink.opacity(0.65))
                            .contentTransition(.numericText())
                    }
                }
            }
        }
        .fixedSize()
        .foregroundStyle(Palette.ink.opacity(0.65))
    }
}

struct MachineDetail: View {
    let gauges: [AlondraContract.Gauge]
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Mac").font(.display(.title3, weight: .bold))
            ForEach(gauges, id: \.id) { gauge in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    ReadingBar(value: gauge.value, tint: gauge.level.tint)
                    Text(gauge.detail ?? "\(gauge.label): \(gauge.text)")
                        .foregroundStyle(gauge.level == .hot ? Palette.rose : Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }.font(.subheadline).padding(20).frame(minWidth: 260, alignment: .leading)
    }
}

/// A toolbar measures its items without a width, which would let `ViewThatFits` always pick its
/// widest choice; this hands the content the slot's width instead.
private struct ProposedWidth: ViewModifier {
    let width: CGFloat
    func body(content: Content) -> some View { WidthLayout(width: width) { content } }
}

private struct WidthLayout: Layout {
    let width: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        subviews.first?.sizeThatFits(ProposedViewSize(width: min(proposal.width ?? width, width), height: proposal.height)) ?? .zero
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        subviews.first?.place(at: bounds.origin, proposal: ProposedViewSize(width: bounds.width, height: bounds.height))
    }
}

private struct ReadingBar: View {
    let value: Double
    let tint: Color
    var body: some View {
        Capsule().fill(Palette.ink.opacity(0.15)).frame(width: 22, height: 4)
            .overlay(alignment: .leading) {
                Capsule().fill(tint).frame(width: 22 * max(0.06, min(value, 1)))
            }
    }
}

private extension Level {
    var tint: Color {
        switch self {
        case .calm: Palette.sky
        case .warm: Palette.dawn
        case .hot: Palette.rose
        }
    }
}
