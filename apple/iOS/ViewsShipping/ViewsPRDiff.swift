import AlondraShared
import AlondraViewsDiff
import AlondraViews
import SwiftUI

/// The verified diff content uses the same file and line presentation as the prior PR screen.
/// Drafts stay local until the server-declared review action is submitted.
struct ViewsPRDiff: View {
    let files: [AlondraViewsDiff.DiffFile]
    @Binding var comments: [String: ActionComment]
    @Environment(\.colorScheme) private var colorScheme
    @State private var collapsed: Set<String>
    @State private var highlighted: [String: AttributedString] = [:]
    @State private var editing: AlondraViewsDiff.DiffLine?
    @State private var editingText = ""

    init(text: String, comments: Binding<[String: ActionComment]>) {
        let files = AlondraViewsDiff.UnifiedDiff.parse(text)
        self.files = files
        self._comments = comments
        self._collapsed = State(initialValue: Set(files.filter(\.startsCollapsed).map(\.id)))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Diff").font(.title3.weight(.bold))
                Spacer()
                Button(collapsed.count == files.count ? "Expand All Files" : "Collapse All Files",
                       systemImage: collapsed.count == files.count ? "rectangle.expand.vertical" : "rectangle.compress.vertical") {
                    withAnimation(.snappy) {
                        collapsed = collapsed.count == files.count ? [] : Set(files.map(\.id))
                    }
                }
                .font(.caption)
            }
            Text("Tap a line to comment on it").font(.caption).foregroundStyle(.secondary)
            LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                ForEach(files) { file in
                    Section {
                        if !collapsed.contains(file.id) {
                            ForEach(file.lines) { line in
                                Button {
                                    editingText = line.anchor.flatMap { comments[$0]?.text } ?? ""
                                    editing = line
                                } label: {
                                    VStack(alignment: .leading, spacing: 0) {
                                        AlondraViewsDiff.DiffCodeLine(line: line, coloured: highlighted[line.id])
                                        if let comment = line.anchor.flatMap({ comments[$0] }) {
                                            Label(comment.text, systemImage: "text.bubble.fill")
                                                .font(.caption).padding(8)
                                                .frame(maxWidth: .infinity, alignment: .leading)
                                                .background(Color.accentColor.opacity(0.12))
                                        }
                                    }
                                    .contentShape(.rect)
                                }
                                .buttonStyle(.plain)
                                .disabled(line.anchor == nil)
                            }
                        }
                    } header: {
                        AlondraViewsDiff.DiffFileHeader(file: file,
                                       comments: file.commentCount(in: comments.mapValues(\.text)),
                                       collapsed: collapsed.contains(file.id)) {
                            withAnimation(.snappy) { collapsed.formSymmetricDifference([file.id]) }
                        }
                    }
                }
            }
        }
        .task(id: colorScheme) {
            var rendered: [String: AttributedString] = [:]
            for file in files {
                guard !Task.isCancelled else { return }
                rendered.merge(await AlondraViewsDiff.DiffHighlighter.highlight(file, dark: colorScheme == .dark)) { _, new in new }
            }
            guard !Task.isCancelled else { return }
            highlighted = rendered
        }
        .sheet(item: $editing) { line in
            AlondraViewsDiff.LineCommentSheet(line: line,
                             initial: line.anchor.flatMap { comments[$0]?.text } ?? "",
                             text: $editingText) { value in
                guard let anchor = line.anchor else { return }
                if value.isEmpty { comments.removeValue(forKey: anchor) }
                else { comments[anchor] = ActionComment(anchor: anchor, text: value, quote: line.text) }
            }
            .presentationDetents([.medium, .large])
        }
        .sensoryFeedback(.success, trigger: comments.count) { old, new in new > old }
    }
}
