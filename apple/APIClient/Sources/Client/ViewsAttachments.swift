import CryptoKit
import Foundation

/// The immutable upload receipt used by Views actions.
public struct ViewUploadedAttachment: Codable, Hashable, Sendable {
    public let id: String
    public let name: String
    public let contentDigest: String
    public let contentReference: String
    public let bytes: UInt64

    enum CodingKeys: String, CodingKey {
        case id, name, bytes
        case contentDigest = "content_digest"
        case contentReference = "content_reference"
    }
}

public extension AlondraAPIClient {
    /// Resumable, idempotent upload. Every chunk uses the same transfer identity on retry.
    func uploadViewAttachment(_ data: Data, name: String, transferID: UUID,
                              progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> ViewUploadedAttachment {
        guard !data.isEmpty, data.count <= 20 * 1024 * 1024 else { throw AttachmentTransferError.invalidSize }
        guard !name.isEmpty, name.utf8.count <= 1024 else { throw AttachmentTransferError.invalidName }
        let id = transferID.uuidString.lowercased()
        let hash = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        struct Start: Encodable { let transferId: String; let name: String; let bytes: Int; let sha256: String }
        struct Chunk: Encodable { let transferId: String; let offset: Int; let data: String }
        struct Finish: Encodable { let transferId: String }
        struct Started: Decodable {
            let offset: Int
            let chunkBytes: Int
            let completed: Bool
            enum CodingKeys: String, CodingKey { case offset, chunkBytes = "chunk_bytes", completed }
        }
        struct Advanced: Decodable { let offset: Int }
        struct Finished: Decodable { let attachment: ViewUploadedAttachment }
        let decoder = JSONDecoder()
        let started = try decoder.decode(Started.self, from: try await send(
            viewUploadEndpoint("start", Start(transferId: id, name: name, bytes: data.count, sha256: hash)), operationID: id + "-start"))
        guard (0...data.count).contains(started.offset), started.chunkBytes == 64 * 1024,
              !started.completed || started.offset == data.count else { throw AttachmentTransferError.invalidResponse }
        progress(Double(started.offset) / Double(data.count))
        var offset = started.offset
        while offset < data.count {
            try Task.checkCancellation()
            let end = min(offset + 64 * 1024, data.count)
            let bytes = data.subdata(in: offset..<end).base64EncodedString()
                .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
                .replacingOccurrences(of: "=", with: "")
            let advanced = try decoder.decode(Advanced.self, from: try await send(
                viewUploadEndpoint("chunk", Chunk(transferId: id, offset: offset, data: bytes)), operationID: id + "-chunk-\(offset)"))
            guard advanced.offset >= end, advanced.offset <= data.count else { throw AttachmentTransferError.invalidResponse }
            offset = advanced.offset
            progress(Double(offset) / Double(data.count))
        }
        try Task.checkCancellation()
        let stored = try decoder.decode(Finished.self, from: try await send(
            viewUploadEndpoint("finish", Finish(transferId: id)), operationID: id + "-finish")).attachment
        guard !stored.id.isEmpty, stored.bytes == UInt64(data.count), stored.contentDigest == hash, stored.name == name,
              !stored.contentReference.isEmpty else { throw AttachmentTransferError.invalidResponse }
        return stored
    }

    func cancelViewUpload(_ transferID: UUID) async throws {
        struct Cancel: Encodable { let transferId: String }
        let id = transferID.uuidString.lowercased()
        _ = try await send(viewUploadEndpoint("cancel", Cancel(transferId: id)), operationID: id + "-cancel")
    }

    private func viewUploadEndpoint<Body: Encodable>(_ operation: String, _ body: Body) throws -> Endpoint<Data> {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return .data(.post, path: ["uploads", operation], body: try encoder.encode(body),
                     contentType: "application/json", requiresJSONObjectResponse: true)
    }
}
