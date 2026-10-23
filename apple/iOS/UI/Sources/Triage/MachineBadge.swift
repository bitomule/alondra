import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// The pill shares the navigation bar with Settings. A toolbar item wider than its slot is folded,
/// with Settings, into a "…" menu, so the pill gives up numbers before it outgrows the slot: first
/// the machine figure, then every figure, keeping each reading's icon.
/// The dot says whether the Mac is answering; its figures fade while they are not current.
struct MachineBadge: View {
    let gauges: [AlondraContract.Gauge]
    let light: ConnectionIndicators.Light
    let width: CGFloat

    static let reservedBarWidth: CGFloat = 120

    var body: some View {
        ViewThatFits(in: .horizontal) {
            pill { _ in true }
            pill { !Self.machineLoad.contains($0) }
            pill { _ in false }
        }
        .modifier(ProposedWidth(width: max(width - Self.reservedBarWidth, 0)))
        .accessibilityValue(lightDescription)
    }

    private var lightDescription: Text {
        switch light {
        case .live: Text("Connected")
        case .connecting: Text("Connecting…")
        case .down: Text("Not connected")
        }
    }

    private var lightColor: Color {
        switch light {
        case .live: Palette.sage
        case .connecting: Palette.ink.opacity(0.4)
        case .down: Palette.rose
        }
    }

    private static let machineLoad = MachineCapacity.sources.union([MachineCapacity.id])

    private func pill(showsText: @escaping (String) -> Bool) -> some View {
        HStack(spacing: 6) {
            Circle().fill(lightColor).frame(width: 8, height: 8)
                .shadow(color: lightColor.opacity(light == .live ? 0.8 : 0), radius: 4)
                .accessibilityIdentifier("mac.light")
            figures(showsText: showsText)
                .opacity(light == .live ? 1 : 0.45)
        }
        .fixedSize()
        .foregroundStyle(Palette.ink.opacity(0.65))
    }

    @ViewBuilder private func figures(showsText: @escaping (String) -> Bool) -> some View {
        HStack(spacing: 6) {
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
    }
}

public struct MachineDetail: View {
    let gauges: [AlondraContract.Gauge]

    public init(gauges: [AlondraContract.Gauge]) { self.gauges = gauges }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Mac").font(.display(.title3, weight: .bold))
            ForEach(gauges, id: \.id) { gauge in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        ReadingIcon(gauge: gauge).frame(width: 22)
                        Text(gauge.parts == nil ? gauge.detail ?? "\(gauge.label): \(gauge.text)" : "\(gauge.label) \(gauge.text)")
                            .fontWeight(gauge.parts == nil ? .regular : .semibold)
                            .foregroundStyle(gauge.level == .hot ? Palette.rose : Palette.ink)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let parts = gauge.parts {
                        ForEach(Array(parts.enumerated()), id: \.offset) { _, part in
                            GaugePartRow(part: part).padding(.leading, 32)
                        }
                        if let detail = gauge.detail {
                            Text(detail).font(.caption).foregroundStyle(Palette.ink.opacity(0.6))
                                .fixedSize(horizontal: false, vertical: true).padding(.leading, 32)
                        }
                    }
                }
            }
        }.font(.subheadline).padding(20).frame(minWidth: 260, alignment: .leading)
    }
}

/// One account of the Claude margin: its name and margin, then what that margin comes from.
private struct GaugePartRow: View {
    let part: GaugePart

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Text(part.label).fontWeight(.medium)
                Spacer(minLength: 12)
                Text(part.text).monospacedDigit()
                    .foregroundStyle(part.level == .hot ? Palette.rose : part.level == .warm ? Palette.dawn : Palette.ink.opacity(0.65))
            }
            if let detail = part.detail {
                Text(detail).font(.caption).foregroundStyle(Palette.ink.opacity(0.6)).fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .combine)
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
        case "claude_margin": ("battery.0percent", "battery.100percent")
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
