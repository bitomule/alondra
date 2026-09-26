import AlondraShared
import AlondraQuestionsFeature
import ComposableArchitecture
import SwiftUI

struct ServiceReviewableDocument: View {
    let text: String
    let anchorPrefix: String
    let store: StoreOf<DocumentReviewFeature>
    var focus: FocusState<DetailField?>.Binding

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(MarkdownParser.segments(from: text)) { segment in
                switch segment {
                case .block(let block) where block.isCommentable:
                    ServiceCommentableBlock(block: block, target: CommentTarget(block: block, prefix: anchorPrefix), store: store, focus: focus)
                case .block(let block):
                    BlockView(block: block).padding(.horizontal, 10)
                case .table(let rows):
                    ServiceCommentableTable(rows: rows, anchorPrefix: anchorPrefix, store: store, focus: focus)
                }
            }
        }
    }
}

struct ServiceCommentableBlock: View {
    let block: MarkdownBlock
    let target: CommentTarget
    let store: StoreOf<DocumentReviewFeature>
    var focus: FocusState<DetailField?>.Binding
    @State private var hovering = false

    var body: some View {
        let comment = store.draft.comments.first(where: { $0.anchor == target.anchor })
        let editing = store.composingAnchor == target.anchor
        VStack(alignment: .leading, spacing: 6) {
            BlockView(block: block)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(highlight(comment: comment != nil, editing: editing), in: .rect(cornerRadius: 6))
                .overlay(alignment: .leading) {
                    if comment != nil || editing {
                        Capsule().fill(Color.accentColor).frame(width: 3).padding(.vertical, 2)
                    }
                }
                .overlay(alignment: .topLeading) {
                    CommentButton(commented: comment != nil, action: startEditing)
                        .opacity(hovering && !editing ? 1 : 0)
                        .offset(x: -28, y: 1)
                }
                .serviceCommentActions(comment: comment, store: store, start: startEditing)

            if editing {
                ServiceDocumentCommentEditor(store: store, anchor: target.anchor, quote: block.plainText, position: target.position, focus: focus)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            } else if let comment {
                CommentCallout(comment: comment, edit: startEditing, remove: { store.send(.commentChanged(anchor: comment.anchor, quote: comment.quote, text: "")) })
                    .transition(.opacity)
            }
        }
        .onHover { hovering = $0 }
        .animation(.snappy(duration: 0.18), value: editing)
        .animation(.snappy(duration: 0.18), value: comment)
    }

    private func highlight(comment: Bool, editing: Bool) -> Color {
        if editing || comment { return Color.accentColor.opacity(0.09) }
        return hovering ? Color.primary.opacity(0.04) : .clear
    }

    private func startEditing() {
        store.send(.composeComment(target.anchor))
    }
}

struct ServiceCommentableTable: View {
    let rows: [MarkdownBlock]
    let anchorPrefix: String
    let store: StoreOf<DocumentReviewFeature>
    var focus: FocusState<DetailField?>.Binding
    @State private var hovered: Int?

    var body: some View {
        TableGrid(rows: rows) { block, column, cell in
            let target = CommentTarget(block: block, prefix: anchorPrefix)
            let comment = store.draft.comments.first(where: { $0.anchor == target.anchor })
            let editing = store.composingAnchor == target.anchor
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if column == 0 {
                    CommentButton(commented: comment != nil) { store.send(.composeComment(target.anchor)) }
                        .opacity(hovered == block.id && !editing ? 1 : 0)
                }
                cell
            }
            .frame(maxHeight: .infinity, alignment: .top)
            .background(highlight(block, commented: comment != nil || editing))
            .onHover { inside in
                if inside {
                    hovered = block.id
                } else if hovered == block.id {
                    hovered = nil
                }
            }
            .serviceCommentActions(comment: comment, store: store) { store.send(.composeComment(target.anchor)) }
        } below: { block in
            let target = CommentTarget(block: block, prefix: anchorPrefix)
            if store.composingAnchor == target.anchor {
                ServiceDocumentCommentEditor(store: store, anchor: target.anchor, quote: block.plainText, position: target.position, focus: focus)
                    .padding(.vertical, 4)
                    .gridCellUnsizedAxes(.horizontal)
            } else if let comment = store.draft.comments.first(where: { $0.anchor == target.anchor }) {
                CommentCallout(comment: comment, edit: { store.send(.composeComment(target.anchor)) }, remove: { store.send(.commentChanged(anchor: comment.anchor, quote: comment.quote, text: "")) })
                    .padding(.vertical, 4)
                    .gridCellUnsizedAxes(.horizontal)
            }
        }
        .padding(.leading, -18)
        .padding(.trailing, 10)
        .animation(.snappy(duration: 0.18), value: store.composingAnchor)
        .animation(.snappy(duration: 0.18), value: store.draft.comments)
    }

    private func highlight(_ block: MarkdownBlock, commented: Bool) -> Color {
        if commented { return Color.accentColor.opacity(0.09) }
        return hovered == block.id ? Color.primary.opacity(0.04) : .clear
    }
}

struct ServiceCommentsSummary: View {
    let comments: [ReviewComment]
    let remove: (ReviewComment) -> Void

    var body: some View {
        if !comments.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("\(comments.count) comments")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(comments.enumerated()), id: \.element.id) { index, comment in
                            HStack(alignment: .firstTextBaseline, spacing: 8) {
                                Text(verbatim: "\(index + 1).")
                                    .monospacedDigit()
                                    .foregroundStyle(.secondary)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text("“\(ReviewFeedback.quote(comment.quote))”")
                                        .italic()
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                    Text(comment.text)
                                        .lineLimit(2)
                                }
                                Spacer(minLength: 0)
                                Button {
                                    remove(comment)
                                } label: {
                                    Image(systemName: "xmark.circle.fill")
                                }
                                .buttonStyle(.borderless)
                                .foregroundStyle(.tertiary)
                                .help("Remove comment")
                                .accessibilityLabel("Remove comment \(index + 1)")
                            }
                            .font(.callout)
                        }
                    }
                }
                .scrollBounceBehavior(.basedOnSize)
                .frame(height: comments.count > 2 ? 104 : nil)
                .fixedSize(horizontal: false, vertical: comments.count <= 2)
            }
            .transition(.opacity.combined(with: .move(edge: .bottom)))
        }
    }
}


extension View {
    func serviceCommentActions(comment: ReviewComment?, store: StoreOf<DocumentReviewFeature>, start: @escaping () -> Void) -> some View {
        contextMenu {
            Button(comment == nil ? String(localized: "Comment…") : String(localized: "Edit Comment…"), action: start)
            if let comment {
                Button("Remove Comment", role: .destructive) { store.send(.commentChanged(anchor: comment.anchor, quote: comment.quote, text: "")) }
            }
        }
        .accessibilityAction(named: comment == nil ? String(localized: "Comment") : String(localized: "Edit Comment"), start)
    }
}
struct ServiceDocumentCommentEditor: View {
    let store: StoreOf<DocumentReviewFeature>
    let anchor: String
    let quote: String
    var position: String?
    var focus: FocusState<DetailField?>.Binding
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("What do you want to say about this?", text: Binding(get: { store.composingText }, set: { store.send(.composingTextChanged($0)) }), axis: .vertical)
                .textFieldStyle(.plain).lineLimit(2...10)
                .focused(focus, equals: .comment).onSubmit(save).onExitCommand(perform: cancel)
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
    private func save() {
        store.send(.saveComposedComment(anchor: anchor, quote: quote, position: position ?? anchor))
        focus.wrappedValue = .pane
    }
    private func cancel() { store.send(.composeComment(nil)); focus.wrappedValue = .pane }
}
