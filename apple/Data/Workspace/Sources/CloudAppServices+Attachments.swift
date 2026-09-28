import AlondraDomain
import AlondraAttachmentStorage
import AlondraAPIClient
import AlondraContracts
import Foundation

extension CloudAppServices {
    public func downloadAttachment(_ attachment: DomainV2.AttachmentRef, downloadID: UUID) async throws -> DownloadedAttachment {
        try checkActive()
        let bytes = try await api.domainAttachmentContent(attachment)
        try checkActive()
        try Task.checkCancellation()
        let file = try await attachmentStorage.store(bytes, name: attachment.name)
        do {
            try checkActive()
            try Task.checkCancellation()
            return file
        } catch {
            await attachmentStorage.release(file)
            throw error
        }
    }

    public func releaseAttachment(_ file: DownloadedAttachment) async {
        await attachmentStorage.release(file)
    }

    public func uploadAttachment(_ data: Data, name: String, transferID: UUID,
                                 progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> DomainV2.AttachmentRef {
        try checkActive()
        let stored = try await api.uploadDomainAttachment(data, name: name, transferID: transferID, progress: progress)
        try checkActive()
        return stored
    }
}
