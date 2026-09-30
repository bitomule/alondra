import AlondraShared
import SwiftUI

struct ServiceBlockRow: View {
    let block: MarkdownBlock
    let comment: String?
    let tint: Color
    let onTap: () -> Void

    var body: some View {
        if block.isCommentable {
            Button(action: onTap) {
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
            }
            .buttonStyle(BlockButtonStyle(tint: tint))
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, -12)
            .accessibilityHint(comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment"))
        } else {
            MarkdownBlockView(block: block)
        }
    }
}

struct ServiceTableRows: View {
    let rows: [MarkdownBlock]
    let comments: [String: String]
    let tint: Color
    let onTap: (MarkdownBlock) -> Void

    var body: some View {
        TableBlock(rows: rows) { block, cell in
            let comment = comments[block.draftKey]
            Button { onTap(block) } label: {
                cell
                    .frame(maxHeight: .infinity, alignment: .top)
                    .background(comment == nil ? .clear : tint.opacity(0.14))
            }
            .buttonStyle(.plain)
            .accessibilityHint(comment == nil ? String(localized: "Add comment") : String(localized: "Edit comment"))
        } below: { block in
            if let comment = comments[block.draftKey] {
                bubble(comment)
                    .padding(.vertical, 4)
                    .gridCellUnsizedAxes(.horizontal)
                    .onTapGesture { onTap(block) }
            }
        } card: { block, card in
            let comment = comments[block.draftKey]
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

struct ServiceCommentSheet: View {
    let block: MarkdownBlock
    let initial: String
    let onSave: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @Binding var text: String
    @FocusState private var focused: Bool

    init(block: MarkdownBlock, initial: String, text: Binding<String>, onSave: @escaping (String) -> Void) {
        self.block = block
        self.initial = initial
        self._text = text
        self.onSave = onSave
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    HStack(alignment: .top, spacing: 12) {
                        Capsule().fill(Palette.dawn).frame(width: 3)
                        Text(ReviewFeedback.quote(block.plainText))
                            .font(.callout.italic())
                            .foregroundStyle(.secondary)
                            .lineLimit(6)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    TextField("What would you change here?", text: $text, axis: .vertical)
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
                        Button("Delete", systemImage: "trash", role: .destructive) {
                            onSave("")
                            dismiss()
                        }
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") {
                        onSave(text.trimmingCharacters(in: .whitespacesAndNewlines))
                        dismiss()
                    }
                    .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationBackground(.regularMaterial)
        .onAppear {
            focused = true
        }
    }
}

private struct ServiceGlassEmphasis: ViewModifier {
    let prominent: Bool

    func body(content: Content) -> some View {
        if prominent {
            content.buttonStyle(.glassProminent)
        } else {
            content.buttonStyle(.glass)
        }
    }
}
