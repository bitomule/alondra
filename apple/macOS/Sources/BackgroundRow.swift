import AlondraShared
import SwiftUI

struct BackgroundRow: View {
    let item: BackgroundItem

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: item.isSuspect ? "exclamationmark.triangle.fill" : item.kind.symbol)
                .foregroundStyle(item.isSuspect ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                .help(item.kind.label)
            Text(verbatim: item.label)
                .font(.system(.callout, design: .monospaced))
                .lineLimit(2)
                .textSelection(.enabled)
            Spacer()
            if item.isSuspect {
                Text(verbatim: BackgroundItem.possiblyHung)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.orange)
            }
            Text("\(ago(item.startedAt)) ago")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}

extension BackgroundKind {
    var symbol: String {
        switch self {
        case .agent: "person.2"
        case .shell: "terminal"
        case .other: "gearshape"
        }
    }
}
