import AlondraShared
import Observation
import SwiftUI

@MainActor
@Observable
final class ReviewDraft {
    var comments: [ReviewComment] = []
    var general = ""
    var editing: String?
    var editorText = ""
    var attachments = Attachments()
    @ObservationIgnored private var positions: [String: String] = [:]

    func feedback(author: String) -> String {
        ReviewFeedback.format(comments, general: general, author: author)
    }

    var hasFeedback: Bool {
        !feedback(author: "").isEmpty
    }

    func comment(for anchor: String) -> ReviewComment? {
        comments.first { $0.anchor == anchor }
    }

    func beginEditing(_ anchor: String) {
        editorText = comment(for: anchor)?.text ?? ""
        editing = anchor
    }

    func commit(anchor: String, quote: String, position: String? = nil) {
        if let position { positions[anchor] = position }
        let text = editorText.trimmingCharacters(in: .whitespacesAndNewlines)
        if let index = comments.firstIndex(where: { $0.anchor == anchor }) {
            if text.isEmpty {
                comments.remove(at: index)
            } else {
                comments[index].text = text
            }
        } else if !text.isEmpty {
            comments.append(ReviewComment(anchor: anchor, quote: quote, text: text))
            comments.sort { Self.order(positions[$0.anchor] ?? $0.anchor, positions[$1.anchor] ?? $1.anchor) }
        }
        cancelEditing()
    }

    func cancelEditing() {
        editing = nil
        editorText = ""
    }

    func remove(_ comment: ReviewComment) {
        comments.removeAll { $0.id == comment.id }
    }

    private static func order(_ lhs: String, _ rhs: String) -> Bool {
        lhs.compare(rhs, options: .numeric) == .orderedAscending
    }
}

@MainActor
@Observable
final class Drafts {
    @ObservationIgnored private var drafts: [String: ReviewDraft] = [:]

    func draft(for key: String) -> ReviewDraft {
        if let draft = drafts[key] { return draft }
        let draft = ReviewDraft()
        drafts[key] = draft
        return draft
    }

    func discard(_ key: String) {
        drafts[key] = nil
    }

    func discardAll() {
        drafts.removeAll()
    }
}

enum DetailField: Hashable {
    case pane
    case reply
    case comment
}

extension View {
    func triageKeys(focus: FocusState<DetailField?>.Binding, handle: @escaping (Character) -> Bool) -> some View {
        focusable()
            .focusEffectDisabled()
            .focused(focus, equals: .pane)
            .onKeyPress(phases: .down) { press in
                guard focus.wrappedValue == .pane || focus.wrappedValue == nil,
                      press.modifiers.subtracting([.numericPad, .capsLock]).isEmpty,
                      let character = press.characters.lowercased().first
                else { return .ignored }
                return handle(character) ? .handled : .ignored
            }
    }
}

struct ReviewableDocument: View {
    let text: String
    let anchorPrefix: String
    let draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(MarkdownParser.segments(from: text)) { segment in
                switch segment {
                case .block(let block) where block.isCommentable:
                    CommentableBlock(block: block, target: CommentTarget(block: block, prefix: anchorPrefix), draft: draft, focus: focus)
                case .block(let block):
                    BlockView(block: block).padding(.horizontal, 10)
                case .table(let rows):
                    CommentableTable(rows: rows, anchorPrefix: anchorPrefix, draft: draft, focus: focus)
                }
            }
        }
    }
}

struct CommentTarget {
    let anchor: String
    let position: String

    init(block: MarkdownBlock, prefix: String) {
        anchor = "\(prefix)-\(block.draftKey)"
        position = "\(prefix)-\(block.id)"
    }
}

struct CommentableBlock: View {
    let block: MarkdownBlock
    let target: CommentTarget
    let draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding
    @State private var hovering = false

    var body: some View {
        let comment = draft.comment(for: target.anchor)
        let editing = draft.editing == target.anchor
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
                .commentActions(comment: comment, draft: draft, start: startEditing)

            if editing {
                CommentEditor(draft: draft, anchor: target.anchor, quote: block.plainText, position: target.position, focus: focus)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            } else if let comment {
                CommentCallout(comment: comment, edit: startEditing, remove: { draft.remove(comment) })
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
        draft.beginEditing(target.anchor)
    }
}

struct CommentableTable: View {
    let rows: [MarkdownBlock]
    let anchorPrefix: String
    let draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding
    @State private var hovered: Int?

    var body: some View {
        TableGrid(rows: rows) { block, column, cell in
            let target = CommentTarget(block: block, prefix: anchorPrefix)
            let comment = draft.comment(for: target.anchor)
            let editing = draft.editing == target.anchor
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if column == 0 {
                    CommentButton(commented: comment != nil) { draft.beginEditing(target.anchor) }
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
            .commentActions(comment: comment, draft: draft) { draft.beginEditing(target.anchor) }
        } below: { block in
            let target = CommentTarget(block: block, prefix: anchorPrefix)
            if draft.editing == target.anchor {
                CommentEditor(draft: draft, anchor: target.anchor, quote: block.plainText, position: target.position, focus: focus)
                    .padding(.vertical, 4)
                    .gridCellUnsizedAxes(.horizontal)
            } else if let comment = draft.comment(for: target.anchor) {
                CommentCallout(comment: comment, edit: { draft.beginEditing(target.anchor) }, remove: { draft.remove(comment) })
                    .padding(.vertical, 4)
                    .gridCellUnsizedAxes(.horizontal)
            }
        }
        .padding(.leading, -18)
        .padding(.trailing, 10)
        .animation(.snappy(duration: 0.18), value: draft.editing)
        .animation(.snappy(duration: 0.18), value: draft.comments)
    }

    private func highlight(_ block: MarkdownBlock, commented: Bool) -> Color {
        if commented { return Color.accentColor.opacity(0.09) }
        return hovered == block.id ? Color.primary.opacity(0.04) : .clear
    }
}

struct CommentButton: View {
    let commented: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: commented ? "text.bubble.fill" : "plus.bubble")
                .font(.system(size: 13))
                .frame(width: 22, height: 22)
                .contentShape(.rect)
        }
        .buttonStyle(.borderless)
        .foregroundStyle(Color.accentColor)
        .help("Comment on this block")
        .accessibilityHidden(true)
    }
}

extension View {
    func commentActions(comment: ReviewComment?, draft: ReviewDraft, start: @escaping () -> Void) -> some View {
        contextMenu {
            Button(comment == nil ? String(localized: "Comment…") : String(localized: "Edit Comment…"), action: start)
            if let comment {
                Button("Remove Comment", role: .destructive) { draft.remove(comment) }
            }
        }
        .accessibilityAction(named: comment == nil ? String(localized: "Comment") : String(localized: "Edit Comment"), start)
    }
}

struct CommentEditor: View {
    @Bindable var draft: ReviewDraft
    let anchor: String
    let quote: String
    var position: String?
    var focus: FocusState<DetailField?>.Binding

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("What do you want to say about this?", text: $draft.editorText, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(2...10)
                .focused(focus, equals: .comment)
                .onSubmit(save)
                .onExitCommand(perform: cancel)
            HStack(spacing: 8) {
                Text("↩ saves · ⌥↩ new line · esc cancels")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                Spacer()
                Button("Cancel", action: cancel)
                    .controlSize(.small)
                Button("Save", action: save)
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
            }
        }
        .padding(10)
        .background(.background, in: .rect(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.accentColor.opacity(0.6)))
        .padding(.leading, 10)
        .task { focus.wrappedValue = .comment }
    }

    private func save() {
        draft.commit(anchor: anchor, quote: quote, position: position)
        focus.wrappedValue = .pane
    }

    private func cancel() {
        draft.cancelEditing()
        focus.wrappedValue = .pane
    }
}

struct CommentCallout: View {
    let comment: ReviewComment
    let edit: () -> Void
    let remove: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "text.bubble.fill")
                .foregroundStyle(Color.accentColor)
                .font(.caption)
            Text(comment.text)
                .font(.callout)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
            Button("Edit", action: edit)
                .buttonStyle(.link)
                .font(.caption)
            Button(action: remove) {
                Image(systemName: "xmark")
                    .font(.caption2.weight(.bold))
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .help("Remove comment")
            .accessibilityLabel("Remove Comment")
        }
        .padding(.leading, 20)
        .padding(.trailing, 6)
    }
}

struct CommentsSummary: View {
    let draft: ReviewDraft

    var body: some View {
        if !draft.comments.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("\(draft.comments.count) comments")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(draft.comments.enumerated()), id: \.element.id) { index, comment in
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
                                    draft.remove(comment)
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
                .frame(height: draft.comments.count > 2 ? 104 : nil)
                .fixedSize(horizontal: false, vertical: draft.comments.count <= 2)
            }
            .transition(.opacity.combined(with: .move(edge: .bottom)))
        }
    }
}

struct AttachmentDocument: View {
    let attachment: Attachment
    let anchorPrefix: String
    let store: FleetStore
    let draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding
    @State private var content: Result<String, Error>?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "doc.text")
                    .foregroundStyle(.secondary)
                Text(attachment.name)
                    .font(.callout.weight(.medium))
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Open") {
                    NSWorkspace.shared.open(URL(fileURLWithPath: attachment.storedPath ?? attachment.sourcePath))
                }
                .buttonStyle(.link)
                .font(.callout)
                .help("Open with the default app")
            }
            .padding(.horizontal, 10)
            Divider()
            switch content {
            case .success(let text):
                ReviewableDocument(text: text, anchorPrefix: anchorPrefix, draft: draft, focus: focus)
            case .failure(let error):
                Label("Couldn’t read it: \(error.localizedDescription)", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 10)
            case nil:
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, minHeight: 60)
            }
        }
        .task(id: attachment.storedPath) {
            guard let path = attachment.storedPath else { return }
            do {
                content = .success(try await store.document(at: path))
            } catch is CancellationError {
                return
            } catch {
                content = .failure(error)
            }
        }
    }
}
