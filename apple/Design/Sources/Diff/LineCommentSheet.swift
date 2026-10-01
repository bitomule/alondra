import SwiftUI

/// Writes, edits or clears the comment on one diff line.
public struct LineCommentSheet: View {
    let line: DiffLine
    let initial: String
    let onSave: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    private let text: Binding<String>
    @FocusState private var focused: Bool

    /// The feature owns the editable buffer; the view only renders and forwards editing.
    public init(line: DiffLine, initial: String, text: Binding<String>, onSave: @escaping (String) -> Void) {
        self.line = line
        self.initial = initial
        self.onSave = onSave
        self.text = text
    }

    private var trimmed: String { text.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines) }

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
                    TextField(String(localized: "What should change here?", table: "AlondraDesign", bundle: #bundle), text: text, axis: .vertical)
                        .lineLimit(4...12)
                        .focused($focused)
                }
                if !initial.isEmpty {
                    Section {
                        Button(String(localized: "Delete Comment", table: "AlondraDesign", bundle: #bundle), role: .destructive) {
                            onSave("")
                            dismiss()
                        }
                    }
                }
            }
            .navigationTitle(initial.isEmpty
                ? String(localized: "Comment", table: "AlondraDesign", bundle: #bundle)
                : String(localized: "Edit Comment", table: "AlondraDesign", bundle: #bundle))
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(String(localized: "Cancel", table: "AlondraDesign", bundle: #bundle), systemImage: "xmark", role: .cancel) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(String(localized: "Save", table: "AlondraDesign", bundle: #bundle), systemImage: "checkmark") {
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
