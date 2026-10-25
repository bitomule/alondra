import SwiftUI

/// The bar over a file's diff: fold chevron, what happened to the file, its path and its lines in and out.
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
        HStack(spacing: 8) {
            if let collapsed {
                Image(systemName: collapsed ? "chevron.right" : "chevron.down")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.secondary)
                    .frame(width: 12)
                    .accessibilityHidden(true)
            }
            DiffStatusBadge(file: file)
            HStack(spacing: 0) {
                if !folder.isEmpty {
                    Text(verbatim: folder)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Text(verbatim: name)
                    .fontWeight(.semibold)
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            .font(.caption.monospaced())
            Spacer(minLength: 6)
            if comments > 0 {
                Label {
                    Text(verbatim: "\(comments)")
                } icon: {
                    Image(systemName: "text.bubble.fill")
                }
                .labelStyle(.titleAndIcon)
                .font(.caption2)
                .foregroundStyle(Color.accentColor)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(String(localized: "\(comments) comments", table: "AlondraDesign", bundle: #bundle))
            }
            DiffCounts(file: file)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(.bar)
    }

    /// The folder in grey and the file name in bold; a renamed file shows where it came from.
    private var name: String { file.path.split(separator: "/").last.map(String.init) ?? file.path }

    private var folder: String {
        let parts = file.path.split(separator: "/")
        let directory = parts.count > 1 ? parts.dropLast().joined(separator: "/") + "/" : ""
        return file.oldPath.map { "\($0) → \(directory)" } ?? directory
    }
}
