import AlondraAttachments

extension Connection {
    public nonisolated static func attachmentImportClient() -> AttachmentImportClient {
        AttachmentImportClient(file: { try AttachmentImporter.file($0) },
                               photo: { try await AttachmentImporter.photo($0) },
                               camera: { try AttachmentImporter.camera($0) },
                               thumbnail: { await AttachmentImporter.thumbnail($0) })
    }
}
