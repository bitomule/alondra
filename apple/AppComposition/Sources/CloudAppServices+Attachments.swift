import AlondraAttachmentsData
import AlondraAttachmentsFeature
import AlondraAPIClient
import Foundation

extension CloudAppServices {
    public nonisolated static func attachmentImportClient() -> AttachmentImportClient {
        AttachmentImportClient(file: { try AttachmentImporter.file($0) }, photo: { try await AttachmentImporter.photo($0) }, camera: { try AttachmentImporter.camera($0) })
    }

    public func uploadAttachment(_ data: Data, name: String, transferID: UUID, progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> String {
        try checkActive()
        let stored = try await api.uploadAttachment(data, name: name, transferID: transferID, progress: progress)
        try checkActive()
        return stored
    }
}
