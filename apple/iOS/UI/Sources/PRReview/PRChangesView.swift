import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// A pull request's changes on their own screen: the files GitHub-style, a list to jump to any of
/// them, long lines wrapped or scrolled sideways, and, where the PR takes line comments, a tap on a
/// line to comment on it.
public struct PRChangesView: View {
    let files: [DiffFile]?
    let failed: Bool
    let truncated: Bool
    let collapsed: Set<String>
    let comments: [String: ActionComment]?
    let marks: [String: FileViewed]?
    let failedMark: String?
    let toggle: (String) -> Void
    let toggleViewed: (String) -> Void
    let dismissFailure: () -> Void
    let toggleAll: () -> Void
    let comment: (String, ActionComment?) -> Void
    let retry: () -> Void

    @AppStorage("diff.wrapsLines") private var wrapsLines = true
    @Environment(\.highlightsDiffs) private var highlightsDiffs
    @State private var target: String?
    @State private var showsFiles = false
    @State private var editing: DiffLine?
    @State private var editingText = ""

    public init(files: [DiffFile]?, failed: Bool = false, truncated: Bool = false, collapsed: Set<String>,
                comments: [String: ActionComment]?, marks: [String: FileViewed]? = nil, failedMark: String? = nil,
                toggle: @escaping (String) -> Void, toggleViewed: @escaping (String) -> Void = { _ in },
                dismissFailure: @escaping () -> Void = {}, toggleAll: @escaping () -> Void,
                comment: @escaping (String, ActionComment?) -> Void, retry: @escaping () -> Void) {
        self.files = files
        self.failed = failed
        self.truncated = truncated
        self.collapsed = collapsed
        self.comments = comments
        self.marks = marks
        self.failedMark = failedMark
        self.toggle = toggle
        self.toggleViewed = toggleViewed
        self.dismissFailure = dismissFailure
        self.toggleAll = toggleAll
        self.comment = comment
        self.retry = retry
    }

    public var body: some View {
        content
            .background(DawnBackground(glow: Palette.sky))
            .navigationTitle("Changes")
            .navigationSubtitle(subtitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { toolbar }
            .safeAreaInset(edge: .bottom) {
                VStack(spacing: 8) {
                    if let failedMark {
                        ViewedFailureNotice(path: failedMark, dismiss: dismissFailure)
                    }
                    commentHint
                }
            }
            .sheet(isPresented: $showsFiles) { fileList }
            .sheet(item: $editing, content: commentSheet)
            .sensoryFeedback(.success, trigger: commentCount) { old, new in new > old }
    }

    @ViewBuilder private var content: some View {
        if let files {
            DiffViewer(files: files, collapsed: collapsed, wrapsLines: wrapsLines, truncated: truncated,
                       highlights: highlightsDiffs, comments: commentTexts, marks: marks, target: $target,
                       toggle: toggle, toggleViewed: toggleViewed, onLine: lineAction)
        } else if failed {
            ContentUnavailableView {
                Label("Couldn’t load the changes", systemImage: "exclamationmark.triangle")
            } actions: {
                Button("Try Again", action: retry).buttonStyle(.glass)
            }
        } else {
            ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var subtitle: Text {
        guard let files else { return Text(verbatim: "") }
        return DiffSummary(files).line
    }

    private var commentTexts: [String: String] {
        guard let comments else { return [:] }
        return comments.mapValues(\.text)
    }

    private var lineAction: ((DiffLine) -> Void)? {
        guard comments != nil else { return nil }
        return { line in edit(line) }
    }

    private func commentSheet(_ line: DiffLine) -> some View {
        let initial = line.anchor.flatMap { comments?[$0]?.text } ?? ""
        return LineCommentSheet(line: line, initial: initial, text: $editingText) { value in
            guard let anchor = line.anchor else { return }
            comment(anchor, value.isEmpty ? nil : ActionComment(anchor: anchor, text: value, quote: line.text))
        }
        .presentationDetents([.medium, .large])
    }

    private var commentCount: Int { comments.map { $0.count } ?? 0 }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        if let files, !files.isEmpty {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Files", systemImage: "list.bullet") { showsFiles = true }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu("Options", systemImage: "ellipsis") {
                    Toggle("Wrap Lines", systemImage: "text.word.spacing", isOn: $wrapsLines)
                    if collapsed.count == files.count {
                        Button("Expand All", systemImage: "chevron.down") { withAnimation(.snappy) { toggleAll() } }
                    } else {
                        Button("Collapse All", systemImage: "chevron.up") { withAnimation(.snappy) { toggleAll() } }
                    }
                }
            }
        }
    }

    @ViewBuilder private var commentHint: some View {
        if let comments, files?.isEmpty == false {
            Group {
                if comments.isEmpty {
                    Text("Tap a line to comment on it")
                } else {
                    Text("\(comments.count) comments")
                }
            }
            .font(.footnote.weight(.medium)).foregroundStyle(.secondary)
            .padding(.horizontal, 14).padding(.vertical, 8)
            .glassEffect(.regular, in: .capsule)
            .padding(.bottom, 8)
        }
    }

    private var fileList: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(files ?? []) { file in
                        Button {
                            showsFiles = false
                            if collapsed.contains(file.id), file.hasCode, !file.isBinary { toggle(file.id) }
                            target = file.id
                        } label: {
                            DiffFileRow(file: file, viewed: marks?[file.id])
                        }
                        .buttonStyle(.plain)
                    }
                } header: {
                    if let viewed = DiffSummary.viewedCount(files ?? [], marks: marks) { viewed }
                }
            }
            .navigationTitle("Files")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close", systemImage: "xmark") { showsFiles = false }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func edit(_ line: DiffLine) {
        editingText = line.anchor.flatMap { comments?[$0]?.text } ?? ""
        editing = line
    }
}
