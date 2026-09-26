import AlondraDomain
import AlondraAttachmentStorage
import AlondraAPIClient
import Foundation

extension CloudAppServices {
    public func downloadAttachment(path: String, name: String, downloadID: UUID) async throws -> DownloadedAttachment {
        try await downloadAttachment(path: path, name: name, downloadID: downloadID, storage: attachmentStorage)
    }

    public func releaseAttachment(_ file: DownloadedAttachment) async {
        await attachmentStorage.release(file)
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
