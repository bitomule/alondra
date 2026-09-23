import AlondraShared
import SwiftUI

/// The week's Claude usage, today against its share, memory and CPU as small bars in the
/// toolbar; hovering one says what it is.
struct StatusGauges: View {
    let machine: Machine
    let usage: Usage?

    var body: some View {
        HStack(spacing: 12) {
            ForEach(StatusReadings.of(machine: machine, usage: usage)) { reading in
                HStack(spacing: 5) {
                    ReadingBar(value: reading.value, tint: tint(reading.level))
                    Text(verbatim: reading.label)
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(reading.level == .hot ? .red : .secondary)
                }
                .help(reading.help)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(reading.help)
            }
        }
        .padding(.horizontal, 6)
    }

    private func tint(_ level: StatusReading.Level) -> Color {
        switch level {
        case .calm: .accentColor
        case .warm: .orange
        case .hot: .red
        }
    }
}

private struct ReadingBar: View {
    let value: Double
    let tint: Color

    var body: some View {
        Capsule()
            .fill(.quaternary)
            .frame(width: 28, height: 4)
            .overlay(alignment: .leading) {
                Capsule()
                    .fill(tint)
                    .frame(width: 28 * max(0.06, min(value, 1)))
            }
    }
}
