import AlondraContract
import SwiftUI

/// A card's image drawn in the card: it downloads as soon as it shows, and a tap opens it full screen.
struct ImageTile: View {
    let attachment: Attachment
    let files: CardAttachmentFiles
    let height: CGFloat
    let open: (URL) -> Void
    @State private var thumbnail: CGImage?

    var body: some View {
        let phase = files.phase(attachment)
        ZStack {
            Palette.dusk
            switch phase {
            case .ready(let file):
                if let thumbnail {
                    Button { open(file) } label: {
                        Image(decorative: thumbnail, scale: 1).resizable().scaledToFill()
                            .frame(maxWidth: .infinity, minHeight: height, maxHeight: height).clipped()
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(Text("Image, \(attachment.name), \(attachment.sizeText)"))
                    .accessibilityHint(Text("Opens it full screen"))
                    .accessibilityIdentifier("cardAttachment.image.\(attachment.id)")
                } else {
                    ProgressView()
                }
            case .idle, .loading:
                ProgressView().accessibilityLabel(Text("Downloading \(attachment.name)"))
            case .failed(let failure):
                VStack(spacing: 8) {
                    Image(systemName: "photo").font(.title2).foregroundStyle(.secondary)
                    Text(attachment.name).font(.footnote.weight(.medium)).lineLimit(1).truncationMode(.middle)
                    AttachmentFailureNote(failure: failure) { files.load(attachment) }
                        .font(.caption).foregroundStyle(Palette.rose).multilineTextAlignment(.center)
                }
                .padding(12)
            }
        }
        .frame(maxWidth: .infinity, minHeight: height, maxHeight: height)
        .clipShape(.rect(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Palette.hairline))
        .onAppear { files.load(attachment) }
        .task(id: phase) {
            guard case .ready(let file) = phase, thumbnail == nil else { return }
            thumbnail = await AttachmentPreviewImage.image(at: file, maxPixels: 1200)
        }
    }
}
