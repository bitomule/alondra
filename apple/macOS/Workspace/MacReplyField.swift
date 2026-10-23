import AlondraContract
import AlondraFeatures
import SwiftUI

/// The field of a Mac card that writes to the node: Return sends, Escape leaves it.
struct MacReplyField: View {
    let action: AlondraContract.Action
    let tint: Color
    @Binding var text: String
    let focus: FocusState<Bool>.Binding
    let send: () -> Void

    private var empty: Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(action.replyPlaceholder, text: $text, axis: .vertical)
                .textFieldStyle(.plain).lineLimit(5)
                .focused(focus)
                .onSubmit(send)
                .onKeyPress(.escape) { focus.wrappedValue = false; return .handled }
            Button(action: send) {
                Image(systemName: "arrow.up.circle.fill").font(.title3)
                    .foregroundStyle(empty ? Color.secondary.opacity(0.5) : tint)
            }
            .buttonStyle(.plain)
            .disabled(empty)
            .help("Send the reply")
            .accessibilityLabel("Send the reply")
        }
        .padding(.horizontal, 12).padding(.vertical, 9)
        .background(MacCardPalette.inset, in: .rect(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous)
            .strokeBorder(focus.wrappedValue ? tint.opacity(0.7) : MacCardPalette.hairline))
    }
}
