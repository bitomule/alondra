import AlondraDomain
import AlondraShared
import CoreTransferable
import Foundation
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

public enum AttachmentImportError: LocalizedError {
    case invalidSize, unavailable
    public var errorDescription: String? {
        switch self {
        case .invalidSize: "Choose a non-empty file no larger than 20 MiB."
        case .unavailable: "The attachment could not be loaded."
        }
    }
}
public enum AttachmentImporter {
    public static func file(_ url: URL) throws -> AttachmentDraft {
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard (1...20 * 1024 * 1024).contains(size) else { throw AttachmentImportError.invalidSize }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let bytes = try handle.read(upToCount: 20 * 1024 * 1024 + 1) ?? Data()
        guard (1...20 * 1024 * 1024).contains(bytes.count) else { throw AttachmentImportError.invalidSize }
        let prepared = AttachmentFile.prepare(bytes, name: url.lastPathComponent)
        guard prepared.data.count <= 20 * 1024 * 1024 else { throw AttachmentImportError.invalidSize }
        return AttachmentDraft(name: prepared.name, data: prepared.data)
    }
    public static func photo(_ item: PhotosPickerItem) async throws -> AttachmentDraft {
        guard let photo = try await item.loadTransferable(type: ImportedPhoto.self) else { throw AttachmentImportError.unavailable }
        return photo.draft
    }
    private struct ImportedPhoto: Transferable {
        let draft: AttachmentDraft
        static var transferRepresentation: some TransferRepresentation {
            FileRepresentation(importedContentType: .image) { file in
                // Read while the picker owns this URL, with a size check and bounded handle.
                ImportedPhoto(draft: try AttachmentImporter.file(file.file))
            }
        }
    }
}
