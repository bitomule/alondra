import AlondraDesign
import AlondraContract
import SwiftUI

/// The Views PR diff keeps the file and line controls used by the earlier Mac review.
struct MacViewsPRDiff: View {
    let files: [AlondraDesign.DiffFile]
    @Binding var comments: [String: ActionComment]
    @Environment(\.colorScheme) private var colorScheme
    @State private var collapsed: Set<String>
    @State private var highlighted: [String: AttributedString] = [:]
    @State private var editing: AlondraDesign.DiffLine?
    @State private var editingText = ""

    init(text: String, comments: Binding<[String: ActionComment]>) {
        let files = AlondraDesign.UnifiedDiff.parse(text)
        self.files = files
        self._comments = comments
        self._collapsed = State(initialValue: Set(files.filter(\.startsCollapsed).map(\.id)))
    }

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            Button(collapsed.count == files.count ? "Expand All Files" : "Collapse All Files") {
                withAnimation(.snappy(duration: 0.2)) {
                    collapsed = collapsed.count == files.count ? [] : Set(files.map(\.id))
                }
            }
            .buttonStyle(.link)
            .font(.caption)
            LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                ForEach(files) { file in
                    Section {
                        if !collapsed.contains(file.id) {
                            ForEach(file.lines) { line in
                                VStack(alignment: .leading, spacing: 4) {
                                    AlondraDesign.DiffCodeLine(line: line, coloured: highlighted[line.id])
                                        .contentShape(.rect)
                                        .onTapGesture { edit(line) }
                                        .contextMenu {
                                            if let anchor = line.anchor {
                                                Button(comments[anchor].map { _ in String(localized: "Edit Comment…") }
                                                       ?? String(localized: "Comment…")) { edit(line) }
                                            }
                                        }
                                    if let anchor = line.anchor, let comment = comments[anchor] {
                                        Label(comment.text, systemImage: "text.bubble.fill")
                                            .font(.caption).padding(8)
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                            .background(Color.accentColor.opacity(0.12))
                                    }
                                }
                            }
                        }
                    } header: {
                        AlondraDesign.DiffFileHeader(file: file,
                                       comments: file.commentCount(in: comments.mapValues(\.text)),
                                       collapsed: collapsed.contains(file.id)) {
                            withAnimation(.snappy(duration: 0.2)) { collapsed.formSymmetricDifference([file.id]) }
                        }
                    }
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
        }
        .task(id: colorScheme) {
            var rendered: [String: AttributedString] = [:]
            for file in files {
                guard !Task.isCancelled else { return }
                rendered.merge(await AlondraDesign.DiffHighlighter.highlight(file, dark: colorScheme == .dark)) { _, new in new }
            }
            guard !Task.isCancelled else { return }
            highlighted = rendered
        }
        .sheet(item: $editing) { line in
            AlondraDesign.LineCommentSheet(line: line,
                             initial: line.anchor.flatMap { comments[$0]?.text } ?? "",
                             text: $editingText) { value in
                guard let anchor = line.anchor else { return }
                if value.isEmpty { comments.removeValue(forKey: anchor) }
                else { comments[anchor] = ActionComment(anchor: anchor, text: value, quote: line.text) }
            }
            .frame(minWidth: 440, minHeight: 360)
        }
    }

    private func edit(_ line: AlondraDesign.DiffLine) {
        guard let anchor = line.anchor else { return }
        editingText = comments[anchor]?.text ?? ""
        editing = line
    }
}
