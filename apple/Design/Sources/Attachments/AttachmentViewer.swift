import AVKit
import AlondraContract
import SwiftUI

/// An image or a video of a card on its own, filling the screen: the image zooms, the video plays
/// here with the system controls, and both can be shared.
struct AttachmentViewer: View {
    struct Item: Identifiable, Equatable {
        let attachment: Attachment
        let file: URL
        var id: String { attachment.id }
    }

    let item: Item
    @Environment(\.dismiss) private var dismiss
    @State private var picture: PlatformPicture?
    @State private var player: AVPlayer?

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            content
            bar
        }
        .environment(\.colorScheme, .dark)
        .task(id: item.file) { await load() }
        .onDisappear { player?.pause() }
        #if os(macOS)
        .frame(minWidth: 900, idealWidth: 1000, minHeight: 640, idealHeight: 720)
        #endif
    }

    @ViewBuilder private var content: some View {
        switch item.attachment.kind {
        case .video:
            Group {
                if let player {
                    VideoPlayer(player: player)
                        .accessibilityIdentifier("attachmentViewer.video")
                } else {
                    ProgressView().tint(.white)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .ignoresSafeArea(edges: .bottom)
            .padding(.top, 56)
        default:
            Group {
                if let picture {
                    ZoomableImage(image: picture.image)
                        .accessibilityLabel(Text(item.attachment.name))
                        .accessibilityIdentifier("attachmentViewer.image")
                } else {
                    ProgressView().tint(.white)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            #if os(macOS)
            .padding(.top, 56)
            #else
            .ignoresSafeArea()
            #endif
        }
    }

    private func load() async {
        if item.attachment.kind == .video {
            let player = AVPlayer(url: item.file)
            self.player = player
            player.play()
        } else {
            picture = await PlatformPicture.load(item.file)
        }
    }

    private var bar: some View {
        HStack(spacing: 12) {
            Button { dismiss() } label: {
                Label("Close", systemImage: "xmark").labelStyle(.iconOnly).font(.headline)
                    .frame(width: 40, height: 40)
            }
            .buttonStyle(.plain)
            .background(.ultraThinMaterial, in: .circle)
            .keyboardShortcut(.cancelAction)
            .accessibilityIdentifier("attachmentViewer.close")
            Text(item.attachment.name).font(.subheadline.weight(.semibold)).lineLimit(1).truncationMode(.middle)
                .frame(maxWidth: .infinity)
            ShareLink(item: item.file) {
                Label("Share", systemImage: "square.and.arrow.up").labelStyle(.iconOnly).font(.headline)
                    .frame(width: 40, height: 40)
            }
            .buttonStyle(.plain)
            .background(.ultraThinMaterial, in: .circle)
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 16)
        .padding(.top, 8)
    }
}

/// The whole picture, scaled down to a size the screen can still zoom into without holding a huge
/// bitmap in memory.
struct PlatformPicture: @unchecked Sendable {
    #if canImport(UIKit)
    let image: UIImage
    #else
    let image: NSImage
    #endif

    static func load(_ file: URL) async -> PlatformPicture? {
        guard let cg = await AttachmentPreviewImage.image(at: file, maxPixels: 4096) else { return nil }
        #if canImport(UIKit)
        return PlatformPicture(image: UIImage(cgImage: cg))
        #else
        return PlatformPicture(image: NSImage(cgImage: cg, size: CGSize(width: cg.width, height: cg.height)))
        #endif
    }
}
