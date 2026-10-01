import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The PR's diff, file by file with sticky headers that fold the file; tap a line to comment on it.
struct PRDiffView: View {
    let files: [DiffFile]
    let collapsed: Set<String>
    let comments: [String: ActionComment]
    let toggle: (String) -> Void
    let comment: (String, ActionComment?) -> Void

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.highlightsDiffs) private var highlightsDiffs
    @State private var highlighted: [String: AttributedString] = [:]
    @State private var editing: DiffLine?
    @State private var editingText = ""

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
            ForEach(files) { file in
                Section {
                    if !collapsed.contains(file.id) {
                        ForEach(file.lines) { line in lineRow(line) }
                    }
                } header: {
                    DiffFileHeader(file: file, comments: file.commentCount(in: comments.mapValues(\.text)),
                                   collapsed: collapsed.contains(file.id)) {
                        withAnimation(.snappy) { toggle(file.id) }
                    }
                }
            }
        }
        .task(id: colorScheme) {
            guard highlightsDiffs else { return }
            var rendered: [String: AttributedString] = [:]
            for file in files {
                guard !Task.isCancelled else { return }
                rendered.merge(await DiffHighlighter.highlight(file, dark: colorScheme == .dark)) { _, new in new }
            }
            guard !Task.isCancelled else { return }
            highlighted = rendered
        }
        .sheet(item: $editing) { line in
            LineCommentSheet(line: line, initial: line.anchor.flatMap { comments[$0]?.text } ?? "",
                             text: $editingText) { value in
                guard let anchor = line.anchor else { return }
                comment(anchor, value.isEmpty ? nil : ActionComment(anchor: anchor, text: value, quote: line.text))
            }
            .presentationDetents([.medium, .large])
        }
        .sensoryFeedback(.success, trigger: comments.count) { old, new in new > old }
    }

    private func lineRow(_ line: DiffLine) -> some View {
        Button {
            editingText = line.anchor.flatMap { comments[$0]?.text } ?? ""
            editing = line
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                DiffCodeLine(line: line, coloured: highlighted[line.id])
                if let text = line.anchor.flatMap({ comments[$0]?.text }) {
                    Label(text, systemImage: "text.bubble.fill")
                        .font(.caption).padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Palette.dawn.opacity(0.14))
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(line.anchor == nil)
    }
}
