import SwiftUI

/// Writes, edits or clears the comment on one diff line.
public struct LineCommentSheet: View {
    let line: DiffLine
    let initial: String
    let onSave: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @FocusState private var focused: Bool

    /// `initial` seeds the text once; later changes to it are not followed.
    public init(line: DiffLine, initial: String, onSave: @escaping (String) -> Void) {
        self.line = line
        self.initial = initial
        self.onSave = onSave
        _text = State(initialValue: initial)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(verbatim: line.anchor ?? "")
                .font(.caption.monospaced().weight(.semibold))
                .foregroundStyle(.secondary)
            Text(verbatim: line.text)
                .font(.callout.monospaced())
                .lineLimit(4)
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.quaternary.opacity(0.5), in: .rect(cornerRadius: 6))
            TextField(String(localized: "What should change here?", table: "AlondraShared", bundle: #bundle), text: $text, axis: .vertical)
                .lineLimit(3...8)
                .focused($focused)
            HStack {
                if !initial.isEmpty {
                    Button(String(localized: "Delete", table: "AlondraShared", bundle: #bundle), role: .destructive) {
                        onSave("")
                        dismiss()
                    }
                }
                Spacer()
                Button(String(localized: "Cancel", table: "AlondraShared", bundle: #bundle), role: .cancel) { dismiss() }
                Button(String(localized: "Save", table: "AlondraShared", bundle: #bundle)) {
                    onSave(text.trimmingCharacters(in: .whitespacesAndNewlines))
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(minWidth: 380)
        .onAppear { focused = true }
    }
}
