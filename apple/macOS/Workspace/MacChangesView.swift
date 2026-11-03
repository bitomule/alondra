import AlondraContract
import AlondraDesign
import SwiftUI

/// A pull request's changes on the Mac: the files on the left, a click jumps to one, and the diff
/// GitHub-style on the right, where clicking a line comments on it. With `marks`, each file has
/// GitHub's «Viewed» box, and ticking it folds the file.
public struct MacChangesView: View {
    let files: [DiffFile]
    let truncated: Bool
    let highlights: Bool
    let marks: [String: FileViewed]?
    let failedMark: String?
    let toggleViewed: (String) -> Void
    let dismissFailure: () -> Void
    @Binding var comments: [String: ActionComment]

    @AppStorage("mac.diff.wrapsLines") private var wrapsLines = false
    @Environment(\.dismiss) private var dismiss
    @State private var collapsed: Set<String>
    @State private var target: String?
    @State private var selection: String?
    @State private var editing: DiffLine?
    @State private var editingText = ""

    public init(files: [DiffFile], truncated: Bool, highlights: Bool = true, marks: [String: FileViewed]? = nil,
                failedMark: String? = nil, toggleViewed: @escaping (String) -> Void = { _ in },
                dismissFailure: @escaping () -> Void = {}, comments: Binding<[String: ActionComment]>) {
        self.files = files
        self.truncated = truncated
        self.highlights = highlights
        self.marks = marks
        self.failedMark = failedMark
        self.toggleViewed = toggleViewed
        self.dismissFailure = dismissFailure
        _comments = comments
        _collapsed = State(initialValue: Set(files.filter { $0.startsCollapsed || marks?[$0.id] == .viewed }.map(\.id)))
    }

    public var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            HStack(spacing: 0) {
                List(files, selection: $selection) { file in
                    DiffFileRow(file: file, viewed: marks?[file.id]).tag(file.id)
                }
                .listStyle(.sidebar)
                .frame(width: 280)
                Divider()
                DiffViewer(files: files, collapsed: collapsed, wrapsLines: wrapsLines, truncated: truncated, highlights: highlights,
                           comments: comments.mapValues(\.text), marks: marks, target: $target,
                           toggle: { collapsed.formSymmetricDifference([$0]) }, toggleViewed: markViewed, onLine: edit)
            }
        }
        .overlay(alignment: .bottom) {
            if let failedMark {
                ViewedFailureNotice(path: failedMark, dismiss: dismissFailure).padding(.bottom, 12)
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
            if let viewed = DiffSummary.viewedCount(files, marks: marks) {
                Label { viewed } icon: { Image(systemName: "checkmark.square") }
                    .font(.callout).foregroundStyle(.secondary)
            }
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

    private func markViewed(_ id: String) {
        if marks?[id] == .viewed { collapsed.remove(id) } else { collapsed.insert(id) }
        toggleViewed(id)
    }

    private func edit(_ line: DiffLine) {
        guard let anchor = line.anchor else { return }
        editingText = comments[anchor]?.text ?? ""
        editing = line
    }
}
