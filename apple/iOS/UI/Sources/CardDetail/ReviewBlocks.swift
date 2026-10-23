import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

struct BlockRow: View {
    let block: MarkdownBlock
    let comment: String?
    let tint: Color
    let onTap: () -> Void

    private var hint: String {
        comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment")
    }

    var body: some View {
        if !block.isCommentable {
            MarkdownBlockView(block: block)
        } else if block.opensLinks {
            row
                .onTapGesture(perform: onTap)
                .accessibilityAction(named: Text(hint), onTap)
        } else {
            Button(action: onTap) { row }
                .buttonStyle(BlockButtonStyle(tint: tint))
                .accessibilityHint(hint)
        }
    }

    /// A block with links is not a button, so a tap on a link opens it and a tap elsewhere comments.
    private var row: some View {
        VStack(alignment: .leading, spacing: 10) {
            MarkdownBlockView(block: block)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let comment {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "text.bubble.fill")
                        .foregroundStyle(tint)
                    Text(comment)
                        .font(.subheadline)
                        .foregroundStyle(Palette.ink)
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(tint.opacity(0.14), in: .rect(cornerRadius: 12))
                .transition(.asymmetric(insertion: .scale(scale: 0.9, anchor: .top).combined(with: .opacity), removal: .opacity))
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background {
            if comment != nil {
                HStack(spacing: 0) {
                    Capsule().fill(tint).frame(width: 3)
                    Spacer()
                }
                .padding(.vertical, 4)
                .transition(.opacity)
            }
        }
        .contentShape(.rect(cornerRadius: 12))
        .foregroundStyle(Palette.ink)
        .padding(.horizontal, -12)
    }
}

struct TableRows: View {
    let rows: [MarkdownBlock]
    let comments: [String: String]
    let tint: Color
    let onTap: (MarkdownBlock) -> Void

    var body: some View {
        TableBlock(rows: rows) { block, cell in
            let comment = comments[block.anchor]
            let hint = comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment")
            let label = cell
                .frame(maxHeight: .infinity, alignment: .top)
                .background(comment == nil ? .clear : tint.opacity(0.14))
            if cell.text.runs[\.link].contains(where: { link, _ in link != nil }) {
                label
                    .contentShape(.rect)
                    .onTapGesture { onTap(block) }
                    .accessibilityAction(named: Text(hint)) { onTap(block) }
            } else {
                Button { onTap(block) } label: { label }
                    .buttonStyle(.plain)
                    .accessibilityHint(hint)
            }
        } below: { block in
            if let comment = comments[block.anchor] {
                bubble(comment)
                    .padding(.vertical, 4)
                    .gridCellUnsizedAxes(.horizontal)
                    .onTapGesture { onTap(block) }
            }
        } card: { block, card in
            let comment = comments[block.anchor]
            Button { onTap(block) } label: {
                VStack(alignment: .leading, spacing: 6) {
                    card
                        .overlay {
                            if comment != nil {
                                RoundedRectangle(cornerRadius: 12).strokeBorder(tint, lineWidth: 1.5)
                            }
                        }
                    if let comment {
                        bubble(comment)
                    }
                }
                .contentShape(.rect(cornerRadius: 12))
            }
            .buttonStyle(BlockButtonStyle(tint: tint))
            .accessibilityHint(comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment"))
        }
        .foregroundStyle(Palette.ink)
    }

    private func bubble(_ comment: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "text.bubble.fill")
                .foregroundStyle(tint)
            Text(comment)
                .font(.subheadline)
                .foregroundStyle(Palette.ink)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.14), in: .rect(cornerRadius: 10))
    }
}

private struct BlockButtonStyle: ButtonStyle {
    let tint: Color

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(tint.opacity(configuration.isPressed ? 0.12 : 0), in: .rect(cornerRadius: 12))
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .animation(.spring(duration: 0.25), value: configuration.isPressed)
    }
}

public struct CommentSheet: View {
    @Bindable var store: StoreOf<CardDetailFeature>
    let composing: CardDetailFeature.Composing

    @Environment(\.dismiss) private var dismiss
    @FocusState private var focused: Bool

    private var initial: String { store.comments[composing.anchor]?.text ?? "" }
    private var text: Binding<String> {
        Binding(get: { store.composing?.text ?? "" }, set: { store.send(.composingChanged($0)) })
    }

    public init(store: StoreOf<CardDetailFeature>, composing: CardDetailFeature.Composing) {
        self.store = store
        self.composing = composing
    }

    public var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    HStack(alignment: .top, spacing: 12) {
                        Capsule().fill(Palette.dawn).frame(width: 3)
                        Text(ReviewFeedback.quote(composing.quote))
                            .font(.callout.italic())
                            .foregroundStyle(.secondary)
                            .lineLimit(6)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    TextField("What would you change here?", text: text, axis: .vertical)
                        .font(.body)
                        .lineLimit(4...12)
                        .focused($focused)
                }
                .padding(20)
            }
            .navigationTitle(initial.isEmpty ? String(localized: "Comment") : String(localized: "Edit Comment"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                if !initial.isEmpty {
                    ToolbarItem(placement: .destructiveAction) {
                        Button("Delete", systemImage: "trash", role: .destructive) { store.send(.commentDeleted) }
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") { store.send(.commentSaved) }
                        .disabled((store.composing?.text ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationBackground(.regularMaterial)
        .onAppear { focused = true }
    }
}
