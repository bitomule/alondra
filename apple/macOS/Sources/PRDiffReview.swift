import AlondraShared
import SwiftUI

/// A PR's diff reviewed like a plan: hover a line to comment, the comment stays under it, and
/// the drafts live with the card until they are sent.
struct PRDiffReview: View {
    let files: [DiffFile]
    let draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding
    @Environment(\.colorScheme) private var scheme
    @State private var colours = DiffColours()

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
            ForEach(files) { file in
                Section {
                    ForEach(file.lines) { line in
                        CommentableDiffLine(line: line, coloured: colours.lines[line.id], draft: draft, focus: focus)
                    }
                } header: {
                    DiffFileHeader(file: file)
                }
            }
        }
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
        .task(id: scheme) { await colours.load(files, dark: scheme == .dark) }
    }
}

private struct CommentableDiffLine: View {
    let line: DiffLine
    let coloured: AttributedString?
    let draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding
    @State private var hovering = false

    var body: some View {
        let anchor = line.anchor
        let comment = anchor.flatMap { draft.comment(for: $0) }
        let editing = anchor != nil && draft.editing == anchor
        VStack(alignment: .leading, spacing: 4) {
            DiffCodeLine(line: line, coloured: coloured)
                .background(hovering && anchor != nil ? Color.primary.opacity(0.05) : .clear)
                .overlay(alignment: .leading) {
                    if comment != nil || editing {
                        Capsule().fill(Color.accentColor).frame(width: 3)
                    }
                }
                .overlay(alignment: .leading) {
                    if let anchor {
                        Button {
                            draft.beginEditing(anchor)
                        } label: {
                            Image(systemName: comment == nil ? "plus.bubble" : "text.bubble.fill")
                                .font(.system(size: 12))
                                .frame(width: 20, height: 18)
                                .contentShape(.rect)
                        }
                        .buttonStyle(.borderless)
                        .foregroundStyle(Color.accentColor)
                        .opacity(hovering && !editing ? 1 : 0)
                        .offset(x: -24)
                        .help("Comment on this line")
                        .accessibilityHidden(true)
                    }
                }
                .contextMenu {
                    if let anchor {
                        Button(comment == nil ? String(localized: "Comment…") : String(localized: "Edit Comment…")) { draft.beginEditing(anchor) }
                        if let comment {
                            Button("Remove Comment", role: .destructive) { draft.remove(comment) }
                        }
                    }
                }
            if let anchor, editing {
                CommentEditor(draft: draft, anchor: anchor, quote: line.text, focus: focus)
                    .padding(.vertical, 4)
            } else if let comment {
                CommentCallout(comment: comment, edit: { draft.beginEditing(comment.anchor) }, remove: { draft.remove(comment) })
                    .padding(.vertical, 4)
            }
        }
        .onHover { hovering = $0 }
    }
}
