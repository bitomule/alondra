import AlondraShared
import SwiftUI

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

struct CommentTarget {
    let anchor: String
    let position: String

    init(block: MarkdownBlock, prefix: String) {
        anchor = "\(prefix)-\(block.draftKey)"
        position = "\(prefix)-\(block.id)"
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
