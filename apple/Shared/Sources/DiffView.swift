import SwiftUI

/// A PR's files, line by line with old and new numbers; tapping or clicking a line asks to
/// comment on it, and comments show under their line.
public struct DiffView: View {
    let files: [DiffFile]
    let comments: [String: String]
    let onSelect: (DiffLine) -> Void

    public init(files: [DiffFile], comments: [String: String], onSelect: @escaping (DiffLine) -> Void) {
        self.files = files
        self.comments = comments
        self.onSelect = onSelect
    }

    public var body: some View {
        LazyVStack(alignment: .leading, spacing: 16) {
            ForEach(files) { file in
                VStack(alignment: .leading, spacing: 0) {
                    HStack {
                        Text(file.path)
                            .font(.caption.monospaced().weight(.semibold))
                            .lineLimit(1)
                            .truncationMode(.head)
                        Spacer()
                        Text(verbatim: "+\(file.additions) −\(file.deletions)")
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .background(.quaternary.opacity(0.6))
                    ForEach(file.lines) { line in
                        DiffLineRow(line: line, onSelect: onSelect)
                        if let anchor = line.anchor, let comment = comments[anchor] {
                            Text(comment)
                                .font(.caption)
                                .padding(8)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(Color.yellow.opacity(0.18))
                                .onTapGesture { onSelect(line) }
                        }
                    }
                }
                .clipShape(.rect(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
            }
        }
    }
}

private struct DiffLineRow: View {
    let line: DiffLine
    let onSelect: (DiffLine) -> Void

    var body: some View {
        Button {
            onSelect(line)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 0) {
                number(line.old)
                number(line.new)
                Text(verbatim: prefix + line.text)
                    .foregroundStyle(line.kind == .hunk || line.kind == .note ? .secondary : .primary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 6)
                Spacer(minLength: 0)
            }
            .font(.caption.monospaced())
            .padding(.vertical, 1)
            .background(background)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(line.anchor == nil)
        .accessibilityLabel(Text(verbatim: "\(prefix)\(line.text)"))
    }

    private var prefix: String {
        switch line.kind {
        case .added: "+"
        case .removed: "−"
        case .context: " "
        case .hunk, .note: ""
        }
    }

    private var background: Color {
        switch line.kind {
        case .added: .green.opacity(0.16)
        case .removed: .red.opacity(0.16)
        case .hunk: .blue.opacity(0.08)
        case .context, .note: .clear
        }
    }

    private func number(_ n: Int?) -> some View {
        Text(verbatim: n.map(String.init) ?? "")
            .foregroundStyle(.tertiary)
            .frame(width: 38, alignment: .trailing)
    }
}

/// The review's line comments in diff order, ready for `ReviewExport`.
public func orderedComments(_ comments: [String: String], in files: [DiffFile]) -> [(anchor: String, text: String)] {
    var seen = Set<String>()
    return files.flatMap(\.lines).compactMap { line in
        guard let anchor = line.anchor, let text = comments[anchor], seen.insert(anchor).inserted else { return nil }
        return (anchor, text)
    }
}
