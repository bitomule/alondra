import AlondraContract
import SwiftUI
#if canImport(UIKit)
import UIKit
typealias SheetBitmap = UIImage
#else
import AppKit
typealias SheetBitmap = NSImage
#endif

/// A picture Alondra keeps, read the way a card's attachment is read; it keeps its real proportion
/// while it loads, so nothing moves when it arrives.
struct SheetPictureView: View {
    let picture: SheetPicture
    let files: StoredFiles?
    @State private var bitmap: SheetBitmap?
    @State private var failed = false

    init(picture: SheetPicture, files: StoredFiles?) {
        self.picture = picture
        self.files = files
        let cached = files.flatMap { files in URL(string: picture.url).flatMap(StoredImage.attachment).flatMap(files.cached) }
        _bitmap = State(initialValue: cached.flatMap { SheetBitmap(sheetData: $0) })
    }

    var body: some View {
        Group {
            if let bitmap {
                Image(sheetBitmap: bitmap).resizable().scaledToFit()
            } else if failed {
                Image(systemName: "photo").font(.title2).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Palette.night)
        .task(id: picture.url) { await load() }
    }

    private func load() async {
        guard bitmap == nil else { return }
        guard let files, let attachment = URL(string: picture.url).flatMap(StoredImage.attachment),
              let data = try? await files.fetch(attachment), let image = SheetBitmap(sheetData: data) else { failed = true; return }
        bitmap = image
    }
}

extension Image {
    init(sheetBitmap: SheetBitmap) {
        #if canImport(UIKit)
        self.init(uiImage: sheetBitmap)
        #else
        self.init(nsImage: sheetBitmap)
        #endif
    }
}

extension SheetBitmap {
    convenience init?(sheetData: Data) { self.init(data: sheetData) }
}

/// A mockup, or a capture of the evidence, as a thumbnail with its real proportion and its name
/// and size; a tap opens it full screen, where it zooms.
struct SheetMockupView: View {
    let part: SheetPart
    let picture: SheetPicture
    @State private var opened = false
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.storedFiles) private var files

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button { opened = true } label: {
                SheetPictureView(picture: picture, files: files)
                    .frame(width: thumbnail.width, height: thumbnail.height)
                    .clipShape(.rect(cornerRadius: 12, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Palette.hairline))
                    .overlay(alignment: .bottomTrailing) {
                        Image(systemName: "arrow.up.left.and.arrow.down.right").font(.caption.weight(.bold)).foregroundStyle(.white)
                            .padding(7).background(.black.opacity(0.55), in: .circle).padding(8).accessibilityHidden(true)
                    }
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text(part.alt))
            .accessibilityHint(Text("Opens it full screen, where it zooms", tableName: "AlondraDesign", bundle: #bundle))
            .accessibilityAddTraits(.isImage)
            SheetPartCaption(part: part, detail: size)
        }
        #if canImport(UIKit)
        .fullScreenCover(isPresented: $opened) { SheetPictureViewer(part: part, picture: picture, files: files) }
        #else
        .sheet(isPresented: $opened) { SheetPictureViewer(part: part, picture: picture, files: files) }
        #endif
    }

    /// Its real proportion inside a box no taller than 300 points and no wider than 260.
    private var thumbnail: CGSize {
        let tallest: CGFloat = typeSize.isAccessibilitySize ? 220 : 300
        let width = min(260, tallest * picture.aspectRatio)
        return CGSize(width: width, height: width / picture.aspectRatio)
    }

    private var size: String? {
        picture.width > 0 && picture.height > 0 ? "\(picture.width)×\(picture.height)" : nil
    }
}

struct SheetPartCaption: View {
    let part: SheetPart
    var detail: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(part.title).font(.footnote.weight(.semibold)).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
            if let detail { Text(detail).font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
            HStack(spacing: 6) {
                if let mark = part.mark { SheetMarkView(mark: mark) }
                Spacer(minLength: 0)
                SheetAffordances(anchor: part.anchor, edit: part.edit, subject: part.title)
            }
        }
    }
}

/// A picture on its own, filling the screen, zooming with a pinch or a double tap.
public struct SheetPictureViewer: View {
    let part: SheetPart
    let picture: SheetPicture
    @Environment(\.dismiss) private var dismiss
    let files: StoredFiles?
    @State private var bitmap: SheetBitmap?

    public init(part: SheetPart, picture: SheetPicture, files: StoredFiles?) {
        self.part = part
        self.picture = picture
        self.files = files
        let cached = files.flatMap { files in URL(string: picture.url).flatMap(StoredImage.attachment).flatMap(files.cached) }
        _bitmap = State(initialValue: cached.flatMap { SheetBitmap(sheetData: $0) })
    }

    public var body: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            Group {
                if let bitmap {
                    ZoomableImage(image: bitmap)
                        .accessibilityLabel(Text(part.alt))
                        .accessibilityAddTraits(.isImage)
                } else {
                    ProgressView().tint(.white)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            HStack {
                Text(part.title).font(.headline).lineLimit(2).accessibilityAddTraits(.isHeader)
                Spacer()
                Button { dismiss() } label: {
                    Label { Text("Close", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "xmark") }
                        .labelStyle(.iconOnly).font(.headline).frame(width: 44, height: 44)
                        .background(.white.opacity(0.18), in: .circle)
                }
                .buttonStyle(.plain)
            }
            .foregroundStyle(.white).padding(.horizontal, 16).padding(.top, 8)
        }
        .environment(\.colorScheme, .dark)
        #if os(macOS)
        .frame(minWidth: 800, idealWidth: 960, minHeight: 600, idealHeight: 720)
        #endif
        .task(id: picture.url) { await load() }
    }

    private func load() async {
        guard bitmap == nil, let files, let attachment = URL(string: picture.url).flatMap(StoredImage.attachment) else { return }
        if let data = try? await files.fetch(attachment) { bitmap = SheetBitmap(sheetData: data) }
    }
}
