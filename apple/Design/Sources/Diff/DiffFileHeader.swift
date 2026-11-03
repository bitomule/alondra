import AlondraContract
import SwiftUI

/// The bar over a file's diff: fold chevron, what happened to the file, its path, its lines in and
/// out, and GitHub's «Viewed» box when the Mac sent it.
public struct DiffFileHeader: View {
    let file: DiffFile
    let comments: Int
    let collapsed: Bool?
    let viewed: FileViewed?
    let toggle: (() -> Void)?
    let toggleViewed: (() -> Void)?

    public init(file: DiffFile, comments: Int = 0, collapsed: Bool? = nil, viewed: FileViewed? = nil,
                toggle: (() -> Void)? = nil, toggleViewed: (() -> Void)? = nil) {
        self.file = file
        self.comments = comments
        self.collapsed = collapsed
        self.viewed = viewed
        self.toggle = toggle
        self.toggleViewed = toggleViewed
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
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
                if let viewed, let toggleViewed {
                    ViewedBox(viewed: viewed, toggle: toggleViewed)
                }
            }
            if viewed == .changed {
                Label {
                    Text("Changed since you viewed it", tableName: "AlondraDesign", bundle: #bundle)
                } icon: {
                    Image(systemName: "arrow.triangle.2.circlepath")
                }
                .font(.caption2.weight(.medium))
                .foregroundStyle(Palette.dawn)
                .padding(.leading, collapsed == nil ? 0 : 20)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(.bar)
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
    }

    /// The folder in grey and the file name in bold; a renamed file shows where it came from.
    private var name: String { file.path.split(separator: "/").last.map(String.init) ?? file.path }

    private var folder: String {
        let parts = file.path.split(separator: "/")
        let directory = parts.count > 1 ? parts.dropLast().joined(separator: "/") + "/" : ""
        return file.oldPath.map { "\($0) → \(directory)" } ?? directory
    }
}

/// GitHub's «Viewed» box: ticked, it says the person already reviewed the file. On the iPhone
/// only the box shows, so the file's path keeps its room.
struct ViewedBox: View {
    let viewed: FileViewed
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            Label {
                Text("Viewed", tableName: "AlondraDesign", bundle: #bundle)
            } icon: {
                Image(systemName: viewed == .viewed ? "checkmark.square.fill" : "square")
                    .foregroundStyle(viewed == .viewed ? Palette.sage : .secondary)
            }
            #if os(iOS)
            .labelStyle(.iconOnly)
            .font(.body)
            .frame(minWidth: 32, minHeight: 32)
            .contentShape(.rect)
            #else
            .labelStyle(.titleAndIcon)
            .font(.caption.weight(.medium))
            .foregroundStyle(viewed == .viewed ? Palette.ink : .secondary)
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(viewed == .viewed ? Palette.sage.opacity(0.14) : Color.clear, in: .capsule)
            .overlay { Capsule().strokeBorder(Palette.hairline) }
            .contentShape(.capsule)
            #endif
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(viewed == .viewed ? .isSelected : [])
        .sensoryFeedback(.selection, trigger: viewed)
    }
}
