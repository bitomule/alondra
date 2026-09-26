import CryptoKit
import Foundation

public enum AttachmentTransferError: Error, Sendable {
    case invalidSize, invalidName, invalidResponse
}

extension AlondraAPIClient {
    /// The caller retains transferID with the draft. Reusing it resumes the same
    /// encrypted operations; a different payload cannot reuse an existing ID.
    public func uploadAttachment(_ data: Data, name: String, transferID: UUID,
        progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> String {
        guard !data.isEmpty, data.count <= 20 * 1024 * 1024 else { throw AttachmentTransferError.invalidSize }
        guard !name.isEmpty, name.utf8.count <= 1024 else { throw AttachmentTransferError.invalidName }
        let id = transferID.uuidString.lowercased()
        let hash = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        struct Start: Encodable { let transferId: String; let name: String; let bytes: Int; let sha256: String }
        struct Chunk: Encodable { let transferId: String; let offset: Int; let data: String }
        struct Finish: Encodable { let transferId: String }
        struct Stored: Decodable { let id: String; let bytes: Int }
        _ = try await send(attachmentEndpoint("start", Start(transferId: id, name: name, bytes: data.count, sha256: hash)), operationID: id + "-start")
        progress(0)
        var offset = 0
        while offset < data.count {
            try Task.checkCancellation()
            let end = min(offset + 64 * 1024, data.count)
            let bytes = data.subdata(in: offset..<end).base64EncodedString()
                .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
                .replacingOccurrences(of: "=", with: "")
            _ = try await send(attachmentEndpoint("chunk", Chunk(transferId: id, offset: offset, data: bytes)), operationID: id + "-chunk-\(offset)")
            offset = end
            progress(Double(offset) / Double(data.count))
        }
        try Task.checkCancellation()
        let response = try await send(attachmentEndpoint("finish", Finish(transferId: id)), operationID: id + "-finish")
        let stored = try JSONDecoder().decode(Stored.self, from: response)
        guard !stored.id.isEmpty, stored.bytes == data.count else { throw AttachmentTransferError.invalidResponse }
        return stored.id
    }

    /// Explicit user cancellation only. An uncertain finish must first be reconciled.
    public func cancelAttachmentTransfer(_ transferID: UUID) async throws {
        struct Cancel: Encodable { let transferId: String }
        let id = transferID.uuidString.lowercased()
        _ = try await send(attachmentEndpoint("cancel", Cancel(transferId: id)), operationID: id + "-cancel")
    }

    private func attachmentEndpoint<Body: Encodable>(_ operation: String, _ body: Body) throws -> Endpoint<Data> {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return .data(.post, path: ["v1", "service", "uploads", operation],
                     body: try encoder.encode(body), contentType: "application/json")
    }
}
