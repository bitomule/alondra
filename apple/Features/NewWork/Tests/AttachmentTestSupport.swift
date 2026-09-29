import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain

extension AttachmentUploadClient {
    /// Test uploads name the stored file by ID; the reference is otherwise fixed.
    init(ids: @escaping @Sendable (AttachmentDraft) async throws -> String) {
        self.init(upload: { stored(try await ids($0)) })
    }

    init(idsWithProgress: @escaping @Sendable (AttachmentDraft, @escaping @Sendable (Double) -> Void) async throws -> String) {
        self.init(uploadWithProgress: { stored(try await idsWithProgress($0, $1)) })
    }
}
