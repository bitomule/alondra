import AlondraDomain
import AlondraAttachmentsData
import AlondraAttachmentsFeature
import AlondraWorkspace
import Foundation

extension CloudAppServices {
    public nonisolated static func attachmentImportClient() -> AttachmentImportClient {
        AttachmentImportClient(file: { try AttachmentImporter.file($0) }, photo: { try await AttachmentImporter.photo($0) }, camera: { try AttachmentImporter.camera($0) }, thumbnail: { await AttachmentImporter.thumbnail($0) })
    }

    public func attachmentDownloadClient() -> AttachmentDownloadClient {
        return AttachmentDownloadClient(
            download: { [self] path, name, id in
                try await downloadAttachment(path: path, name: name, downloadID: id)
            },
            release: { await self.releaseAttachment($0) }
        )
    }
}
