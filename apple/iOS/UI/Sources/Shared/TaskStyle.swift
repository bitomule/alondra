import AlondraContract
import AlondraDesign
import SwiftUI

/// How a task's state looks: the symbol and tint of the daemon's task icon and tone, as the
/// original app drew a task's state.
enum TaskStyle {
    static func tint(icon: Icon, tone: Tone?) -> Color {
        switch icon {
        case .mission: Palette.sky
        case .action, .pullRequest: Palette.dawn
        case .check: Palette.sage
        case .branch, .warning, .task: Palette.slate
        default: tint(tone: tone)
        }
    }

    static func symbol(icon: Icon, struck: Bool = false) -> String {
        switch icon {
        case .mission: "hammer.fill"
        case .action: "hand.raised.fill"
        case .pullRequest: "arrow.triangle.pull"
        case .branch: "link"
        case .check: "checkmark"
        case .warning: "xmark"
        case .task: struck ? "checkmark" : "circle.dashed"
        default: "circle.dashed"
        }
    }

    static func tint(tone: Tone?) -> Color {
        switch tone {
        case nil, .neutral: Palette.slate
        case .info: Palette.sky
        case .accent, .warning: Palette.dawn
        case .success: Palette.sage
        case .critical: Palette.rose
        case .yourTurn: Palette.ember
        }
    }

    /// A state that arrives as a tone only, such as the one on a task's own screen.
    static func symbol(tone: Tone?) -> String {
        switch tone {
        case .accent: "hammer.fill"
        case .warning: "hand.raised.fill"
        case .info: "arrow.triangle.pull"
        case .success: "checkmark"
        case .yourTurn: "hand.point.right.fill"
        case .critical: "questionmark"
        case nil, .neutral: "circle.dashed"
        }
    }
}

/// A node's state as 8ccd131 drew it, from the tone the daemon gives each of its states.
enum NodeStyle {
    static func tint(tone: Tone?) -> Color {
        switch tone {
        case .info: Palette.sky
        case .warning, .accent: Palette.dawn
        case .yourTurn: Palette.ember
        case .success: Palette.sage
        case .critical: Palette.rose
        case nil, .neutral: Palette.slate
        }
    }

    static func symbol(tone: Tone?) -> String {
        switch tone {
        case .info: "hammer.fill"
        case .warning: "hand.raised.fill"
        case .yourTurn: "hand.point.right.fill"
        case .accent: "arrow.triangle.pull"
        case .success: "checkmark"
        case .critical: "pause.circle"
        case nil, .neutral: "hourglass"
        }
    }
}

struct StateChip: View {
    let label: String
    let tint: Color

    var body: some View {
        Text(verbatim: label)
            .font(.caption.weight(.semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 9)
            .padding(.vertical, 4)
            .background(tint.opacity(0.14), in: .capsule)
            .lineLimit(1)
            .fixedSize()
            .padding(.bottom, 2)
    }
}

/// A state's name in its tint, with how long ago it began.
struct StateLine: View {
    let label: String
    let tint: Color
    let since: Date?

    var body: some View {
        HStack(spacing: 4) {
            Text(verbatim: label)
                .foregroundStyle(tint)
            if let since {
                Text(verbatim: "·")
                    .foregroundStyle(.tertiary)
                RelativeTime(date: since)
                    .foregroundStyle(.tertiary)
            }
        }
        .font(.caption.weight(.semibold))
        .lineLimit(1)
    }
}

struct WaitingBadge: View {
    let count: Int

    var body: some View {
        Text(count, format: .number)
            .font(.caption.weight(.bold).monospacedDigit())
            .foregroundStyle(Palette.onAccent)
            .frame(minWidth: 22, minHeight: 22)
            .background(Palette.dawn, in: .capsule)
            .accessibilityLabel(Text("\(count) waiting for you"))
    }
}
