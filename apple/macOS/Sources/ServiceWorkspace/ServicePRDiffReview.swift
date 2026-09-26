import AlondraShared
import AlondraPullRequestsFeature
import ComposableArchitecture
import SwiftUI

/// A PR's diff reviewed like a plan: hover a line to comment, the comment stays under it, and
/// the drafts live with the card until they are sent.
struct ServicePRDiffReview: View {
    let store: StoreOf<DiffReviewFeature>
    private var files: [DiffFile] { store.files ?? [] }
    var focus: FocusState<DetailField?>.Binding
    @Environment(\.colorScheme) private var scheme
    private var collapsed: Set<String> { store.collapsed }

    var body: some View {
        let allCollapsed = collapsed.count == files.count
        VStack(alignment: .trailing, spacing: 6) {
            Button(allCollapsed ? String(localized: "Expand All Files") : String(localized: "Collapse All Files")) {
                withAnimation(.snappy(duration: 0.2)) { _ = store.send(.toggleAllFiles) }
            }
            .buttonStyle(.link)
            .font(.caption)
            LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                ForEach(files) { file in
                    Section {
                        if !collapsed.contains(file.id) {
                            ForEach(file.lines) { line in
                                ServiceCommentableDiffLine(line: line, store: store, focus: focus)
                            }
                        }
                    } header: {
                        DiffFileHeader(file: file, comments: comments(in: file), collapsed: collapsed.contains(file.id)) {
                            withAnimation(.snappy(duration: 0.2)) { _ = store.send(.toggleFile(file.id)) }
                        }
                    }
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
        }
        .task(id: scheme) { store.send(.colorSchemeChanged(dark: scheme == .dark)) }
    }

    private func comments(in file: DiffFile) -> Int {
        Set(file.lines.compactMap(\.anchor)).filter { store.comments[$0] != nil }.count
    }
}

private struct ServiceCommentableDiffLine: View {
    let line: DiffLine
    let store: StoreOf<DiffReviewFeature>
    var focus: FocusState<DetailField?>.Binding
    @State private var hovering = false

    var body: some View {
        let anchor = line.anchor
        let comment = anchor.flatMap { anchor in store.comments[anchor].map { ReviewComment(anchor: anchor, quote: line.text, text: $0) } }
        let editing = anchor != nil && store.editingAnchor == anchor
        VStack(alignment: .leading, spacing: 4) {
            DiffCodeLine(line: line, coloured: store.highlightedLines[line.id])
                .background(hovering && anchor != nil ? Color.primary.opacity(0.05) : .clear)
                .overlay(alignment: .leading) {
                    if comment != nil || editing {
                        Capsule().fill(Color.accentColor).frame(width: 3)
                    }
                }
                .overlay(alignment: .leading) {
                    if let anchor {
                        Button {
                            store.send(.editLine(anchor))
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
                        Button(comment == nil ? String(localized: "Comment…") : String(localized: "Edit Comment…")) { store.send(.editLine(anchor)) }
                        if let comment {
                            Button("Remove Comment", role: .destructive) { store.send(.removeLineComment(comment.anchor)) }
                        }
                    }
                }
            if anchor != nil, editing {
                ServiceDiffCommentEditor(store: store, focus: focus)
                    .padding(.vertical, 4)
            } else if let comment {
                CommentCallout(comment: comment, edit: { store.send(.editLine(comment.anchor)) }, remove: { store.send(.removeLineComment(comment.anchor)) })
                    .padding(.vertical, 4)
            }
        }
        .onHover { hovering = $0 }
    }
}

private struct ServiceDiffCommentEditor: View {
    let store: StoreOf<DiffReviewFeature>
    var focus: FocusState<DetailField?>.Binding
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("What do you want to say about this?", text: Binding(get: { store.editingText }, set: { store.send(.lineEditorChanged($0)) }), axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(2...10)
                .focused(focus, equals: .comment)
                .onSubmit(save)
                .onExitCommand(perform: cancel)
            HStack(spacing: 8) {
                Text("↩ saves · ⌥↩ new line · esc cancels").font(.caption).foregroundStyle(.tertiary)
                Spacer()
                Button("Cancel", action: cancel).controlSize(.small)
                Button("Save", action: save).buttonStyle(.borderedProminent).controlSize(.small)
            }
        }
        .padding(10)
        .background(.background, in: .rect(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.accentColor.opacity(0.6)))
        .padding(.leading, 10)
        .task { focus.wrappedValue = .comment }
    }
    private func save() { store.send(.saveEditedLine); focus.wrappedValue = .pane }
    private func cancel() { store.send(.cancelLineComment); focus.wrappedValue = .pane }
}
