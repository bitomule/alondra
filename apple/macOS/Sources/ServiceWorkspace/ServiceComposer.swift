import AlondraAttachmentsFeature
import ComposableArchitecture
import SwiftUI

struct ServiceComposer: View {
    let placeholder: String
    @Binding var text: String
    var focus: FocusState<DetailField?>.Binding
    var sendTitle: String? = nil
    var canSend = true
    let submit: () -> Void
    var escape: () -> Void = {}
    var attachmentsStore: StoreOf<AttachmentFeature>? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let attachmentsStore {
                ServiceAttachmentStrip(store: attachmentsStore)
            }
            field
        }
        .modifier(ServiceDropsFiles(store: attachmentsStore))
    }

    private var field: some View {
        HStack(alignment: .bottom, spacing: 8) {
            HStack(alignment: .bottom, spacing: 4) {
                TextField(placeholder, text: $text, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...8)
                    .focused(focus, equals: .reply)
                    .onSubmit { if canSend { submit() } }
                    .onExitCommand {
                        focus.wrappedValue = .pane
                        escape()
                    }
                    .padding(.vertical, 5)
                if let attachmentsStore {
                    ServiceAttachFilesButton(store: attachmentsStore)
                }
            }
            .padding(.leading, 12)
            .padding(.trailing, attachmentsStore == nil ? 12 : 4)
            .padding(.vertical, 4)
            .background(.background, in: .rect(cornerRadius: 10))
            .overlay {
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(focus.wrappedValue == .reply ? Color.accentColor.opacity(0.7) : Color.primary.opacity(0.12))
            }
            if let sendTitle, canSend {
                SendButton(title: sendTitle, action: submit)
                    .transition(.opacity.combined(with: .scale(scale: 0.9, anchor: .trailing)))
            }
        }
        .animation(.snappy(duration: 0.18), value: canSend)
    }
}
