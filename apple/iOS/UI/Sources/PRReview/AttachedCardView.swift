import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// A question or report the agent attached to the PR, with the buttons the daemon offers for it.
struct AttachedCardView: View {
    let card: AlondraContract.Card
    let perform: (WireAction) -> Void
    let open: (AlondraContract.Link) -> Void
    let showAttachment: (AlondraContract.Attachment) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                KindBadge(kind: Kind(card))
                Text(card.title).font(.subheadline.weight(.semibold)).foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.leading)
                Spacer(minLength: 0)
                if let link = card.link {
                    Button { open(link) } label: {
                        Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(.tertiary)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Open")
                }
            }
            if let body = card.body, !body.text.isEmpty {
                MarkdownView(markdown: body).frame(maxWidth: .infinity, alignment: .leading)
            }
            ForEach(card.attachments, id: \.id) { attachment in
                Button { showAttachment(attachment) } label: {
                    Label(attachment.name, systemImage: "paperclip").font(.footnote)
                }
                .buttonStyle(.plain).foregroundStyle(Palette.sky)
            }
            if !card.actions.isEmpty {
                HStack(spacing: 8) {
                    ForEach(card.actions, id: \.id) { action in
                        Button { perform(action) } label: {
                            Text(action.label).frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.glass)
                        .disabled(!action.enabled)
                    }
                }
                .buttonBorderShape(.capsule)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
    }
}
