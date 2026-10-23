import AlondraContract
import SwiftUI
import Synchronization

/// Reads a file the daemon stores, the way a card's attachment is read, and keeps the last ones it
/// read: a document's images come back on screen as it scrolls.
public final class StoredFiles: Sendable {
    private static let kept = 24
    private let load: @Sendable (Attachment) async throws -> Data
    private let read = Mutex<[String: Data]>([:])

    public init(fetch: @escaping @Sendable (Attachment) async throws -> Data) {
        load = fetch
    }

    public func cached(_ attachment: Attachment) -> Data? {
        read.withLock { $0[attachment.fetch] }
    }

    public func fetch(_ attachment: Attachment) async throws -> Data {
        if let data = cached(attachment) { return data }
        let data = try await load(attachment)
        read.withLock { read in
            if read.count >= Self.kept { read.removeAll() }
            read[attachment.fetch] = data
        }
        return data
    }
}

public extension EnvironmentValues {
    @Entry var storedFiles: StoredFiles?
}

/// An image the document's author had in their worktree: Alondra copied it into its files folder by
/// its SHA256 and the document names that copy by the daemon's route.
public enum StoredImage {
    static let route = "/files?path="
    static let extensions: Set<String> = ["png", "jpg", "jpeg", "gif", "webp", "heic", "svg"]

    public static func attachment(_ url: URL) -> Attachment? {
        let written = url.absoluteString
        guard written.hasPrefix(route) else { return nil }
        let reference = String(written.dropFirst(route.count))
        let parts = reference.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0] == "files", parts[1] == "images" else { return nil }
        let name = parts[2].split(separator: ".", omittingEmptySubsequences: false)
        guard name.count == 2, extensions.contains(String(name[1])), name[0].count == 64,
              name[0].allSatisfy({ $0.isHexDigit && !$0.isUppercase }) else { return nil }
        return Attachment(id: String(name[0]), name: String(parts[2]), kind: .image, size: 0,
                          fetch: reference, digest: String(name[0]))
    }
}

struct StoredImageView<Placeholder: View>: View {
    let attachment: Attachment
    let files: StoredFiles
    let placeholder: () -> Placeholder
    @State private var phase: Phase

    init(attachment: Attachment, files: StoredFiles, @ViewBuilder placeholder: @escaping () -> Placeholder) {
        self.attachment = attachment
        self.files = files
        self.placeholder = placeholder
        _phase = State(initialValue: files.cached(attachment).flatMap(Self.image).map { Phase.loaded($0.0, $0.1) } ?? .loading)
    }

    private enum Phase {
        case loading, loaded(Image, CGSize), failed
    }

    var body: some View {
        Group {
            switch phase {
            case .loading:
                ProgressView().frame(maxWidth: .infinity, minHeight: 60)
            case .loaded(let image, let size):
                image.resizable().scaledToFit().frame(maxWidth: size.width, maxHeight: size.height)
            case .failed:
                placeholder()
            }
        }
        .task(id: attachment.fetch) {
            if case .loaded = phase { return }
            guard let data = try? await files.fetch(attachment), let image = Self.image(data) else {
                phase = .failed
                return
            }
            phase = .loaded(image.0, image.1)
        }
    }

    private static func image(_ data: Data) -> (Image, CGSize)? {
        #if canImport(UIKit)
        UIImage(data: data).map { (Image(uiImage: $0), $0.size) }
        #else
        NSImage(data: data).map { (Image(nsImage: $0), $0.size) }
        #endif
    }
}
