import AlondraContract
import AlondraDesign
import SwiftUI

/// A pull request's changes on the Mac: the files on the left, a click jumps to one, and the diff
/// GitHub-style on the right, where clicking a line comments on it.
public struct MacChangesView: View {
    let files: [DiffFile]
    let truncated: Bool
    let highlights: Bool
    @Binding var comments: [String: ActionComment]

    @AppStorage("mac.diff.wrapsLines") private var wrapsLines = false
    @Environment(\.dismiss) private var dismiss
    @State private var collapsed: Set<String>
    @State private var target: String?
    @State private var selection: String?
    @State private var editing: DiffLine?
    @State private var editingText = ""

    public init(files: [DiffFile], truncated: Bool, highlights: Bool = true, comments: Binding<[String: ActionComment]>) {
        self.files = files
        self.truncated = truncated
        self.highlights = highlights
        _comments = comments
        _collapsed = State(initialValue: Set(files.filter(\.startsCollapsed).map(\.id)))
    }

    public var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            HStack(spacing: 0) {
                List(files, selection: $selection) { file in
                    DiffFileRow(file: file).tag(file.id)
                }
                .listStyle(.sidebar)
                .frame(width: 280)
                Divider()
                DiffViewer(files: files, collapsed: collapsed, wrapsLines: wrapsLines, truncated: truncated, highlights: highlights,
                           comments: comments.mapValues(\.text), target: $target,
                           toggle: { collapsed.formSymmetricDifference([$0]) }, onLine: edit)
            }
        }
        .onChange(of: selection) { _, id in
            guard let id, let file = files.first(where: { $0.id == id }) else { return }
            if collapsed.contains(id), file.hasCode, !file.isBinary { collapsed.remove(id) }
            target = id
        }
        .sheet(item: $editing) { line in
            LineCommentSheet(line: line, initial: line.anchor.flatMap { comments[$0]?.text } ?? "", text: $editingText) { value in
                guard let anchor = line.anchor else { return }
                if value.isEmpty { comments.removeValue(forKey: anchor) }
                else { comments[anchor] = ActionComment(anchor: anchor, text: value, quote: line.text) }
            }
            .frame(minWidth: 440, minHeight: 360)
        }
    }

    private var header: some View {
        let summary = DiffSummary(files)
        return HStack(spacing: 12) {
            Text("Changes").font(.title3.weight(.semibold))
            HStack(spacing: 8) {
                summary.fileCount
                Text(verbatim: "+\(summary.additions)").foregroundStyle(Palette.sage)
                Text(verbatim: "−\(summary.deletions)").foregroundStyle(Palette.rose)
                DiffStatBar(additions: summary.additions, deletions: summary.deletions)
            }
            .font(.callout.monospacedDigit())
            .foregroundStyle(.secondary)
            Spacer()
            if !comments.isEmpty {
                Label("\(comments.count) comments", systemImage: "text.bubble.fill")
                    .font(.callout).foregroundStyle(Color.accentColor)
            }
            Toggle("Wrap Lines", isOn: $wrapsLines).toggleStyle(.checkbox)
            if collapsed.count == files.count {
                Button("Expand All") { withAnimation(.snappy(duration: 0.2)) { collapsed = [] } }
            } else {
                Button("Collapse All") { withAnimation(.snappy(duration: 0.2)) { collapsed = Set(files.map(\.id)) } }
            }
            Button("Close") { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
    }

    private func edit(_ line: DiffLine) {
        guard let anchor = line.anchor else { return }
        editingText = comments[anchor]?.text ?? ""
        editing = line
    }
}
