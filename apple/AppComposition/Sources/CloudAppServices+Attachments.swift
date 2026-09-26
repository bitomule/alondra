import AlondraAPIClient
import Foundation

extension CloudAppServices {
    public func uploadAttachment(_ data: Data, name: String, transferID: UUID) async throws -> String {
        try checkActive()
        let stored = try await api.uploadAttachment(data, name: name, transferID: transferID)
        try checkActive()
        return stored
    }
}
