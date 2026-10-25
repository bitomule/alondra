import AlondraContract
import AlondraDesign
import SwiftUI

/// The one line of content a Mac card shows; what is behind it opens in the detail.
struct MacContentRow: View {
    let row: ContentRow
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Image(systemName: row.symbol).foregroundStyle(.secondary).frame(width: 18)
                Text(row.label).font(.subheadline.weight(.semibold))
                Text(row.summary).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                if row.moreCount > 0 {
                    Text("+\(row.moreCount)").font(.caption.monospacedDigit().weight(.semibold)).foregroundStyle(.secondary)
                }
                Spacer(minLength: 4)
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 12).padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(MacCardPalette.inset, in: .rect(cornerRadius: 10, style: .continuous))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .keyboardShortcut(row.kind == "diff" ? KeyboardShortcut("d", modifiers: .command) : nil)
        .help("Show the details")
        .accessibilityLabel("\(row.label), \(row.summary)")
    }
}
