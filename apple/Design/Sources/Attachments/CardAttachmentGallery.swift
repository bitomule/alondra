import AlondraContract
import QuickLook
import SwiftUI

/// The files a card carries, drawn as what they are: images as pictures in the card that open full
/// screen, videos as a frame that downloads and plays here, and anything else as a file that opens
/// in Quick Look or is shared. Every download says how far it is and why it failed.
public struct CardAttachmentGallery: View {
    let attachments: [Attachment]
    @Environment(\.cardAttachmentFiles) private var files
    @State private var viewing: AttachmentViewer.Item?
    @State private var quickLook: URL?

    public init(attachments: [Attachment]) { self.attachments = attachments }

    private var images: [Attachment] { attachments.filter { $0.kind == .image } }
    private var videos: [Attachment] { attachments.filter { $0.kind == .video } }
    private var others: [Attachment] { attachments.filter { $0.kind != .image && $0.kind != .video } }

    public var body: some View {
        if !attachments.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                if let files {
                    imageGrid(files)
                    ForEach(videos, id: \.id) { video in
                        VideoTile(attachment: video, files: files) { viewing = .init(attachment: video, file: $0) }
                    }
                    ForEach(others, id: \.id) { other in
                        FileRow(attachment: other, files: files) { quickLook = $0 }
                    }
                } else {
                    ForEach(attachments, id: \.id) { FileRow.label($0).foregroundStyle(.secondary) }
                }
            }
            .quickLookPreview($quickLook)
            #if os(iOS)
            .fullScreenCover(item: $viewing) { AttachmentViewer(item: $0) }
            #else
            .sheet(item: $viewing) { AttachmentViewer(item: $0) }
            #endif
        }
    }

    @ViewBuilder private func imageGrid(_ files: CardAttachmentFiles) -> some View {
        if images.count == 1, let image = images.first {
            ImageTile(attachment: image, files: files, height: 240) { viewing = .init(attachment: image, file: $0) }
        } else if images.count > 1 {
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 8), GridItem(.flexible(), spacing: 8)], spacing: 8) {
                ForEach(images, id: \.id) { image in
                    ImageTile(attachment: image, files: files, height: 150) { viewing = .init(attachment: image, file: $0) }
                }
            }
        }
    }
}

extension Attachment {
    var sizeText: String { ByteCountFormatter.string(fromByteCount: Int64(clamping: size), countStyle: .file) }
}

/// What a failed download says, and whether trying again can help.
struct AttachmentFailureNote: View {
    let failure: CardAttachmentFiles.Failure
    let retry: () -> Void

    var body: some View {
        switch failure {
        case .unreachable:
            Button(action: retry) {
                Label("Couldn’t download it from your Mac · Retry", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.plain)
        case .gone:
            Label("This file is no longer on your Mac as the card names it", systemImage: "exclamationmark.triangle")
        }
    }
}
