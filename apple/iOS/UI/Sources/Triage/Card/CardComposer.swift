import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The field that writes to the node, with its attach and send buttons.
struct CardComposer: View {
    let store: StoreOf<TriageCardFeature>
    let action: AlondraContract.Action
    let cardID: String
    let tint: Color
    let focus: FocusState<Bool>.Binding

    private var empty: Bool { store.reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            if action.attachments {
                AttachButton(store: store.scope(state: \.attachments, action: \.attachments)).padding(.leading, 6).padding(.bottom, 4)
            }
            let reply = Binding(get: { store.reply }, set: { store.send(.replyChanged($0)) })
            Group {
                if action.repliesSecret {
                    SecureField(action.replyPlaceholder, text: reply).textContentType(.password)
                } else {
                    TextField(action.replyPlaceholder, text: reply, axis: .vertical)
                }
            }
                .lineLimit(1...5).focused(focus)
                .padding(.leading, 16).padding(.vertical, 12)
                .accessibilityIdentifier("triage.reply.\(cardID)")
            Button {
                focus.wrappedValue = false
                store.send(.replySent(action))
            } label: {
                Image(systemName: "arrow.up").font(.body.weight(.bold)).foregroundStyle(Palette.onAccent)
                    .frame(width: 36, height: 36).background(empty ? Palette.slate.opacity(0.5) : tint, in: .circle)
            }
            .buttonStyle(PressStyle())
            .disabled(empty)
            .padding(4)
            .accessibilityLabel("Send")
            .accessibilityIdentifier("triage.send.\(cardID)")
        }
        .background(Palette.cardRaised, in: .rect(cornerRadius: 22, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(Palette.hairline))
    }
}
