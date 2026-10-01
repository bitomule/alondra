import SwiftUI

public struct DiffFileHeader: View {
    let file: DiffFile
    let comments: Int
    let collapsed: Bool?
    let toggle: (() -> Void)?

    public init(file: DiffFile, comments: Int = 0, collapsed: Bool? = nil, toggle: (() -> Void)? = nil) {
        self.file = file
        self.comments = comments
        self.collapsed = collapsed
        self.toggle = toggle
    }

    public var body: some View {
        if let toggle {
            Button(action: toggle) {
                row.contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityValue(collapsed == true
                ? String(localized: "Collapsed", table: "AlondraDesign", bundle: #bundle)
                : String(localized: "Expanded", table: "AlondraDesign", bundle: #bundle))
        } else {
            row
        }
    }

    private var row: some View {
        HStack(spacing: 6) {
            if let collapsed {
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.secondary)
                    .rotationEffect(.degrees(collapsed ? 0 : 90))
                    .accessibilityHidden(true)
            }
            Text(verbatim: file.path)
                .font(.caption.monospaced().weight(.semibold))
                .lineLimit(1)
                .truncationMode(.head)
            Spacer()
            if comments > 0 {
                Label {
                    Text(verbatim: "\(comments)")
                } icon: {
                    Image(systemName: "text.bubble.fill")
                }
                .labelStyle(.titleAndIcon)
                .foregroundStyle(Color.accentColor)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(String(localized: "\(comments) comments", table: "AlondraDesign", bundle: #bundle))
            }
            Text(verbatim: "+\(file.additions)")
                .foregroundStyle(.green)
            Text(verbatim: "−\(file.deletions)")
                .foregroundStyle(.red)
        }
        .font(.caption2.monospaced())
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(.bar)
    }
}
