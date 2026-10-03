import AlondraContract
import AlondraDesign
import SwiftUI

/// The pill shares the navigation bar with Settings. A toolbar item wider than its slot is folded,
/// with Settings, into a "…" menu, so the pill gives up numbers before it outgrows the slot: first
/// the machine figure, then every figure, keeping each reading's icon.
struct MachineBadge: View {
    let gauges: [AlondraContract.Gauge]
    let width: CGFloat

    static let reservedBarWidth: CGFloat = 120

    var body: some View {
        ViewThatFits(in: .horizontal) {
            pill { _ in true }
            pill { !Self.machineLoad.contains($0) }
            pill { _ in false }
        }
        .modifier(ProposedWidth(width: max(width - Self.reservedBarWidth, 0)))
    }

    private static let machineLoad = MachineCapacity.sources.union([MachineCapacity.id])

    private func pill(showsText: @escaping (String) -> Bool) -> some View {
        HStack(spacing: 6) {
            Circle().fill(Palette.sage).frame(width: 8, height: 8)
                .shadow(color: Palette.sage.opacity(0.8), radius: 4)
            if let capacity = MachineCapacity(gauges: gauges) {
                HStack(spacing: 3) {
                    ReadingIcon(symbols: ("desktopcomputer", "desktopcomputer"), value: capacity.value, tint: capacity.level.tint)
                    if showsText(MachineCapacity.id) {
                        Text(capacity.text).font(.caption2.monospacedDigit().weight(.medium))
                            .foregroundStyle(capacity.level == .hot ? Palette.rose : Palette.ink.opacity(0.65))
                            .contentTransition(.numericText())
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(capacity.accessibilityLabel)
            } else {
                Image(systemName: "desktopcomputer").font(.footnote)
            }
            ForEach(gauges.filter { !MachineCapacity.sources.contains($0.id) }, id: \.id) { gauge in
                HStack(spacing: 3) {
                    ReadingIcon(gauge: gauge)
                    if showsText(gauge.id) {
                        Text(gauge.text).font(.caption2.monospacedDigit().weight(.medium))
                            .foregroundStyle(gauge.level == .hot ? Palette.rose : Palette.ink.opacity(0.65))
                            .contentTransition(.numericText())
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(gauge.label) \(gauge.text)")
            }
        }
        .fixedSize()
        .foregroundStyle(Palette.ink.opacity(0.65))
    }
}

public struct MachineDetail: View {
    let gauges: [AlondraContract.Gauge]

    public init(gauges: [AlondraContract.Gauge]) { self.gauges = gauges }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Mac").font(.display(.title3, weight: .bold))
            ForEach(gauges, id: \.id) { gauge in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    ReadingIcon(gauge: gauge).frame(width: 22)
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

private struct MachineCapacity {
    static let id = "machine"
    static let sources: Set<String> = ["memory", "cpu", "disk"]

    let value: Double
    let level: Level
    let text: String
    let accessibilityLabel: String

    init?(gauges: [AlondraContract.Gauge]) {
        let load = gauges.filter { Self.sources.contains($0.id) }
        guard let critical = load.max(by: { ($0.level.severity, $0.value) < ($1.level.severity, $1.value) }) else { return nil }
        value = critical.value
        level = critical.level
        text = "\(Int((critical.value * 100).rounded()))%"
        accessibilityLabel = load.map { "\($0.label) \($0.text)" }.joined(separator: ", ")
    }
}

private struct ReadingIcon: View {
    let symbols: (outline: String, filled: String)
    let value: Double
    let tint: Color

    private static let minimumFill = 0.06

    init(gauge: AlondraContract.Gauge) {
        self.init(symbols: Self.symbols(for: gauge.id), value: gauge.value, tint: gauge.level.tint)
    }

    init(symbols: (outline: String, filled: String), value: Double, tint: Color) {
        self.symbols = symbols
        self.value = value
        self.tint = tint
    }

    var body: some View {
        Image(systemName: symbols.outline)
            .font(.footnote)
            .foregroundStyle(Palette.ink.opacity(0.2))
            .overlay {
                Image(systemName: symbols.filled)
                    .font(.footnote)
                    .foregroundStyle(tint)
                    .mask(alignment: .bottom) {
                        GeometryReader { proxy in
                            Rectangle()
                                .frame(height: proxy.size.height * max(Self.minimumFill, min(value, 1)))
                                .frame(maxHeight: .infinity, alignment: .bottom)
                        }
                    }
            }
            .accessibilityHidden(true)
    }

    static func symbols(for id: String) -> (outline: String, filled: String) {
        switch id {
        case "memory": ("memorychip", "memorychip.fill")
        case "cpu": ("cpu", "cpu.fill")
        case "claude_five_hours": ("hourglass", "hourglass")
        case "claude_today": ("sun.max", "sun.max.fill")
        case "claude_week": ("calendar", "calendar")
        default: ("gauge.with.dots.needle.67percent", "gauge.with.dots.needle.67percent")
        }
    }
}

private extension Level {
    var severity: Int {
        switch self {
        case .calm: 0
        case .warm: 1
        case .hot: 2
        }
    }

    var tint: Color {
        switch self {
        case .calm: Palette.sky
        case .warm: Palette.dawn
        case .hot: Palette.rose
        }
    }
}
