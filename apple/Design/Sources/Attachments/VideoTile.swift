import AlondraContract
import SwiftUI

/// A card's video as a frame in the card: a tap downloads it, showing how far it is and letting it
/// stop, and plays it here once it is on this device.
struct VideoTile: View {
    let attachment: Attachment
    let files: CardAttachmentFiles
    let play: (URL) -> Void
    @State private var poster: CGImage?
    @State private var playWhenReady = false

    var body: some View {
        let phase = files.phase(attachment)
        VStack(alignment: .leading, spacing: 6) {
            ZStack {
                Color.black
                if let poster {
                    Image(decorative: poster, scale: 1).resizable().scaledToFill()
                        .frame(maxWidth: .infinity, maxHeight: .infinity).clipped()
                }
                overlay(phase)
            }
            .aspectRatio(16 / 9, contentMode: .fit)
            .frame(maxWidth: .infinity)
            .clipShape(.rect(cornerRadius: 14, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Palette.hairline))
            HStack(spacing: 6) {
                Image(systemName: "film").foregroundStyle(.secondary)
                Text(attachment.name).font(.footnote.weight(.medium)).lineLimit(1).truncationMode(.middle)
                Text("· \(attachment.sizeText)").font(.footnote).foregroundStyle(.secondary)
                Spacer(minLength: 0)
                if case .ready(let file) = phase {
                    ShareLink(item: file) { Label("Share", systemImage: "square.and.arrow.up").labelStyle(.iconOnly) }
                        .buttonStyle(.plain).foregroundStyle(.secondary)
                }
            }
        }
        .task(id: phase) {
            guard case .ready(let file) = phase else { return }
            if playWhenReady {
                playWhenReady = false
                play(file)
            }
            if poster == nil { poster = await AttachmentPreviewImage.firstFrame(of: file, maxPixels: 1200) }
        }
    }

    @ViewBuilder private func overlay(_ phase: CardAttachmentFiles.Phase) -> some View {
        switch phase {
        case .idle:
            Button {
                playWhenReady = true
                files.load(attachment)
            } label: {
                VStack(spacing: 8) {
                    playGlyph
                    Text("Download and play · \(attachment.sizeText)").font(.footnote.weight(.semibold))
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.white)
            .accessibilityLabel(Text("Video, \(attachment.name), \(attachment.sizeText)"))
            .accessibilityHint(Text("Downloads it from your Mac and plays it"))
            .accessibilityIdentifier("cardAttachment.video.\(attachment.id)")
        case .loading(let fraction):
            VStack(spacing: 10) {
                ZStack {
                    Circle().stroke(.white.opacity(0.25), lineWidth: 4)
                    Circle().trim(from: 0, to: fraction ?? 0.02).stroke(.white, style: StrokeStyle(lineWidth: 4, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                        .animation(.easeOut(duration: 0.2), value: fraction)
                    Button { files.cancel(attachment) } label: {
                        Label("Stop", systemImage: "stop.fill").labelStyle(.iconOnly).font(.headline)
                    }
                    .buttonStyle(.plain)
                }
                .frame(width: 52, height: 52)
                Text(fraction.map { String(localized: "Downloading \($0.formatted(.percent.precision(.fractionLength(0))))") }
                     ?? String(localized: "Downloading…"))
                    .font(.footnote.weight(.semibold)).monospacedDigit()
            }
            .foregroundStyle(.white)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("cardAttachment.downloading.\(attachment.id)")
            .accessibilityValue(Text(fraction?.formatted(.percent.precision(.fractionLength(0))) ?? ""))
        case .ready(let file):
            Button { play(file) } label: {
                playGlyph.frame(maxWidth: .infinity, maxHeight: .infinity).contentShape(.rect)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.white)
            .accessibilityLabel(Text("Play \(attachment.name)"))
            .accessibilityIdentifier("cardAttachment.play.\(attachment.id)")
        case .failed(let failure):
            AttachmentFailureNote(failure: failure) {
                playWhenReady = true
                files.load(attachment)
            }
            .font(.footnote.weight(.medium)).foregroundStyle(.white).multilineTextAlignment(.center).padding(16)
        }
    }

    private var playGlyph: some View {
        Image(systemName: "play.fill").font(.title2)
            .frame(width: 56, height: 56)
            .background(.ultraThinMaterial, in: .circle)
            .environment(\.colorScheme, .dark)
    }
}
