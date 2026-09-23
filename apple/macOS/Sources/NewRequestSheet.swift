import AlondraShared
import SwiftUI

struct NewRequestSheet: View {
    let store: FleetStore
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var sending = false
    @FocusState private var focused: Bool

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("What do you want done?")
                    .font(.title3.weight(.semibold))
                Text("I’ll pass it to the orchestrator: it prepares the mission and launches the node.")
                    .foregroundStyle(.secondary)
            }
            TextEditor(text: $text)
                .font(.body)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minWidth: 520, minHeight: 180)
                .background(.background, in: .rect(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.primary.opacity(0.12)))
                .overlay(alignment: .topLeading) {
                    if text.isEmpty {
                        Text("For example: “Find out why Redraft is slow to open and propose a fix”")
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 8)
                            .allowsHitTesting(false)
                    }
                }
                .focused($focused)
            HStack {
                Hint(text: String(localized: "⌘↩ sends · esc cancels"))
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button {
                    send()
                } label: {
                    if sending {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("Send")
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(sending || trimmed.isEmpty)
            }
        }
        .padding(22)
        .onAppear { focused = true }
    }

    private func send() {
        let request = trimmed
        guard !request.isEmpty else { return }
        sending = true
        Task {
            if await store.newRequest(request) {
                dismiss()
            }
            sending = false
        }
    }
}
