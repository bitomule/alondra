import AlondraContract
import AlondraDesign
import SwiftUI

struct MachineBadge: View {
    let gauges: [AlondraContract.Gauge]
    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(Palette.sage).frame(width: 8, height: 8)
                .shadow(color: Palette.sage.opacity(0.8), radius: 4)
            Image(systemName: "desktopcomputer").font(.footnote)
            ForEach(gauges.filter { $0.id != "disk" }, id: \.id) { gauge in
                HStack(spacing: 3) {
                    ReadingBar(value: gauge.value, tint: gauge.level.tint)
                    Text(gauge.text).font(.caption2.monospacedDigit().weight(.medium))
                        .foregroundStyle(gauge.level == .hot ? Palette.rose : Palette.ink.opacity(0.65))
                        .contentTransition(.numericText())
                }
            }
        }.foregroundStyle(Palette.ink.opacity(0.65))
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
