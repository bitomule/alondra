import SwiftUI

/// Original macOS composer field; its binding and actions are owned by the feature.
struct ServiceComposer: View {
    let placeholder: String
    @Binding var text: String
    var focus: FocusState<DetailField?>.Binding
    let submit: () -> Void
    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(placeholder, text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...8)
                .focused(focus, equals: .reply)
                .onSubmit(submit)
                .onExitCommand { focus.wrappedValue = .pane }
                .padding(.vertical, 5)
                .padding(.horizontal, 12)
                .padding(.vertical, 4)
                .background(.background, in: .rect(cornerRadius: 10))
                .overlay {
                    RoundedRectangle(cornerRadius: 10)
                        .strokeBorder(focus.wrappedValue == .reply ? Color.accentColor.opacity(0.7) : Color.primary.opacity(0.12))
                }
        }
    }
}
