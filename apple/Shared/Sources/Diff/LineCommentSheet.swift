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

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    public var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(verbatim: line.text)
                        .font(.callout.monospaced())
                        .lineLimit(6)
                        .textSelection(.enabled)
                } header: {
                    Text(verbatim: line.anchor ?? "")
                        .font(.caption.monospaced().weight(.semibold))
                        .textCase(nil)
                }
                Section {
                    TextField(String(localized: "What should change here?", table: "AlondraShared", bundle: #bundle), text: $text, axis: .vertical)
                        .lineLimit(4...12)
                        .focused($focused)
                }
                if !initial.isEmpty {
                    Section {
                        Button(String(localized: "Delete Comment", table: "AlondraShared", bundle: #bundle), role: .destructive) {
                            onSave("")
                            dismiss()
                        }
                    }
                }
            }
            .navigationTitle(initial.isEmpty
                ? String(localized: "Comment", table: "AlondraShared", bundle: #bundle)
                : String(localized: "Edit Comment", table: "AlondraShared", bundle: #bundle))
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(String(localized: "Cancel", table: "AlondraShared", bundle: #bundle), systemImage: "xmark", role: .cancel) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(String(localized: "Save", table: "AlondraShared", bundle: #bundle), systemImage: "checkmark") {
                        onSave(trimmed)
                        dismiss()
                    }
                    .disabled(trimmed.isEmpty && initial.isEmpty)
                }
            }
        }
        .onAppear { focused = true }
    }
}
