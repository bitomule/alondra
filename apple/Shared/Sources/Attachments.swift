import CoreGraphics
import Foundation
import ImageIO
import Observation
import UniformTypeIdentifiers

/// The files attached to one message being written. Each is copied into memory the moment it is
/// picked, so the original can go, and uploaded to alondrad right away; a send that finds one
/// not yet stored uploads it itself, so a send retried by the queue retries its uploads too.
@MainActor
@Observable
public final class Attachments {
    public enum State: Equatable, Sendable {
        case uploading(Double)
        case stored(String)
        case failed(String)
    }

    public struct Item: Identifiable {
        public let id = UUID()
        public let name: String
        public let data: Data
        public let isImage: Bool
        public let thumbnail: CGImage?
        public var state: State

        public var size: String {
            ByteCountFormatter.string(fromByteCount: Int64(data.count), countStyle: .file)
        }
    }

    public static let maxBytes = 20 * 1024 * 1024

    public private(set) var items: [Item] = []
    @ObservationIgnored private var uploads: [UUID: Task<String, any Error>] = [:]

    public init() {}

    public var isEmpty: Bool { items.isEmpty }

    public var isUploading: Bool {
        items.contains { if case .uploading = $0.state { true } else { false } }
    }

    /// Adds a file and starts copying it to alondrad. Refused, with the reason, when it is too big.
    @discardableResult
    public func add(_ data: Data, name: String, using client: AlondraClient) -> String? {
        let prepared = AttachmentFile.prepare(data, name: name)
        guard prepared.data.count <= Self.maxBytes else {
            let size = ByteCountFormatter.string(fromByteCount: Int64(prepared.data.count), countStyle: .file)
            return String(localized: "\(name) is \(size): attachments go up to 20 MB", table: "AlondraShared", bundle: #bundle)
        }
        let item = Item(name: prepared.name, data: prepared.data, isImage: prepared.isImage, thumbnail: AttachmentFile.thumbnail(prepared.data), state: .uploading(0))
        items.append(item)
        upload(item.id, using: client)
        return nil
    }

    public func remove(_ id: UUID) {
        uploads.removeValue(forKey: id)?.cancel()
        items.removeAll { $0.id == id }
    }

    public func removeAll() {
        uploads.values.forEach { $0.cancel() }
        uploads = [:]
        items = []
    }

    public func retry(_ id: UUID, using client: AlondraClient) {
        upload(id, using: client)
    }

    /// The stored ids, in order, uploading whatever is not stored yet.
    public func storedIDs(using client: AlondraClient) async throws -> [String] {
        var ids: [String] = []
        for item in items {
            if case .stored(let id) = item.state {
                ids.append(id)
                continue
            }
            let task = uploads[item.id] ?? upload(item.id, using: client)
            ids.append(try await task.value)
        }
        return ids
    }

    @discardableResult
    private func upload(_ id: UUID, using client: AlondraClient) -> Task<String, any Error> {
        guard let item = items.first(where: { $0.id == id }) else {
            return Task { throw CancellationError() }
        }
        set(id, .uploading(0))
        let task = Task { [weak self] in
            do {
                let stored = try await client.upload(item.data, name: item.name) { fraction in
                    Task { @MainActor in self?.progress(id, fraction) }
                }
                self?.set(id, .stored(stored))
                self?.uploads[id] = nil
                return stored
            } catch {
                self?.set(id, .failed(error.friendlyDescription))
                self?.uploads[id] = nil
                throw error
            }
        }
        uploads[id] = task
        return task
    }

    private func progress(_ id: UUID, _ fraction: Double) {
        guard let i = items.firstIndex(where: { $0.id == id }), case .uploading = items[i].state else { return }
        items[i].state = .uploading(fraction)
    }

    private func set(_ id: UUID, _ state: State) {
        guard let i = items.firstIndex(where: { $0.id == id }) else { return }
        items[i].state = state
    }
}

/// What a picked file becomes before it is uploaded. Claude reads images only as JPEG, PNG, GIF
/// or WebP, so anything else that is an image (HEIC from the camera roll, TIFF) becomes JPEG, and
