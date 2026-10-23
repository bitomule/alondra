import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One line of how far the node's work has got: its facts (progress, phase, agents) or a step,
/// subagent or workflow agent with its state.
struct ProgressLine: View {
    let item: TaskDetail.Progress

    var body: some View {
        switch item {
        case .facts(let facts):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(facts.enumerated(), id: \.offset) { _, fact in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(verbatim: fact.label).foregroundStyle(.secondary)
                        Text(verbatim: fact.value)
                            .foregroundStyle(fact.tone.map { TaskStyle.tint(tone: $0) } ?? Palette.ink)
                            .lineLimit(2)
                    }
                    .font(.subheadline)
                }
            }
            .accessibilityElement(children: .combine)
        case .row(let row):
            RowLine(row: row)
        }
    }
}

private struct RowLine: View {
    let row: Row

    private var done: Bool { row.icon == .check || row.struck }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: row.icon == .history ? "circle.dotted" : done ? "checkmark.circle.fill" : "circle")
                .font(.subheadline)
                .foregroundStyle(done ? Palette.sage : Palette.slate)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(verbatim: row.title)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(row.struck ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                    .strikethrough(row.struck)
                    .fixedSize(horizontal: false, vertical: true)
                if let subtitle = row.subtitle {
                    Text(verbatim: subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
            }
            Spacer(minLength: 4)
            VStack(alignment: .trailing, spacing: 3) {
                ForEach(row.badges.enumerated(), id: \.offset) { _, badge in
                    Text(verbatim: badge.text)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(TaskStyle.tint(tone: badge.tone))
                }
                switch row.trailing {
                case .time(let at, let format)?: TimeText(raw: at, format: format).font(.caption).foregroundStyle(.tertiary)
                case .text(let text)?: Text(verbatim: text).font(.caption).foregroundStyle(.tertiary)
                default: EmptyView()
                }
            }
        }
        .accessibilityElement(children: .combine)
    }
}
