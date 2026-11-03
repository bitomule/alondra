import AlondraContract
import SwiftUI

/// A text or any other file of a card: a tap downloads it and opens it in Quick Look, where it can
/// also be shared. A kind this app does not know reads as a file like this one.
struct FileRow: View {
    let attachment: Attachment
    let files: CardAttachmentFiles
    let open: (URL) -> Void
    @State private var openWhenReady = false

    var body: some View {
        let phase = files.phase(attachment)
        VStack(alignment: .leading, spacing: 4) {
            Button { tapped(phase) } label: {
                HStack(spacing: 10) {
                    Self.label(attachment)
                    Spacer(minLength: 0)
                    trailing(phase)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityHint(Text("Opens it with Quick Look"))
            if case .failed(let failure) = phase {
                AttachmentFailureNote(failure: failure) { tapped(phase) }
                    .font(.caption).foregroundStyle(Palette.rose)
            }
        }
        .padding(10)
        .background(Palette.dusk.opacity(0.6), in: .rect(cornerRadius: 12, style: .continuous))
        .contextMenu {
            if case .ready(let file) = phase {
                ShareLink(item: file) { Label("Share", systemImage: "square.and.arrow.up") }
            }
        }
        .task(id: phase) {
            guard case .ready(let file) = phase, openWhenReady else { return }
            openWhenReady = false
            open(file)
        }
    }

    static func label(_ attachment: Attachment) -> some View {
        HStack(spacing: 10) {
            Image(systemName: attachment.kind == .text ? "doc.text" : "doc").font(.title3).foregroundStyle(Palette.dawn)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 1) {
                Text(attachment.name).font(.subheadline.weight(.medium)).lineLimit(1).truncationMode(.middle)
                Text(attachment.sizeText).font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private func tapped(_ phase: CardAttachmentFiles.Phase) {
        if case .ready(let file) = phase { return open(file) }
        openWhenReady = true
        files.load(attachment)
    }

    @ViewBuilder private func trailing(_ phase: CardAttachmentFiles.Phase) -> some View {
        switch phase {
        case .idle, .failed: Image(systemName: "arrow.down.circle").foregroundStyle(.secondary)
        case .loading(let fraction):
            if let fraction { ProgressView(value: fraction).frame(width: 60) } else { ProgressView().controlSize(.small) }
        case .ready: Image(systemName: "eye").foregroundStyle(.secondary)
        }
    }
}
