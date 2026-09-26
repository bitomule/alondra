import AlondraDomain
import AlondraAttachmentsData
import AlondraAttachmentsFeature
import AlondraAPIClient
import Foundation

extension CloudAppServices {
    public nonisolated static func attachmentImportClient() -> AttachmentImportClient {
        AttachmentImportClient(file: { try AttachmentImporter.file($0) }, photo: { try await AttachmentImporter.photo($0) }, camera: { try AttachmentImporter.camera($0) }, thumbnail: { await AttachmentImporter.thumbnail($0) })
    }

    public func attachmentDownloadClient() -> AttachmentDownloadClient {
        let storage = attachmentStorage
        return AttachmentDownloadClient(
            download: { [self] path, name, id in
                try await downloadAttachment(path: path, name: name, downloadID: id, storage: storage)
            },
            release: { await storage.release($0) }
        )
    }

    private func downloadAttachment(path: String, name: String, downloadID: UUID,
                                    storage: AttachmentDownloadStorage) async throws -> DownloadedAttachment {
        try checkActive()
        let bytes = try await api.downloadAttachment(path: path, downloadID: downloadID)
        try checkActive()
        try Task.checkCancellation()
        let file = try await storage.store(bytes, name: name)
        do {
            try checkActive()
            try Task.checkCancellation()
            return file
        } catch {
            await storage.release(file)
            throw error
        }
    }

    public func uploadAttachment(_ data: Data, name: String, transferID: UUID, progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> String {
        try checkActive()
        let stored = try await api.uploadAttachment(data, name: name, transferID: transferID, progress: progress)
        try checkActive()
        return stored
    }
}
