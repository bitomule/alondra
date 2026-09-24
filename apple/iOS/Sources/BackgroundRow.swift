import AlondraShared
import SwiftUI

struct BackgroundRow: View {
    let item: BackgroundItem

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: item.isSuspect ? "exclamationmark.triangle.fill" : item.kind.symbol)
                Text(verbatim: item.kind.label)
                if item.isSuspect {
                    Text(verbatim: "· \(BackgroundItem.possiblyHung)")
                }
                Spacer(minLength: 0)
                Text(verbatim: relative(item.startedAt))
                    .foregroundStyle(.tertiary)
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(item.isSuspect ? Palette.ember : Palette.slate)
            Text(verbatim: item.label)
                .font(.mono(.footnote))
                .foregroundStyle(Palette.ink)
                .lineLimit(3)
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
