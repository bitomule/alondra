import CryptoKit
import Foundation

public enum AttachmentTransferError: Error, Equatable, Sendable {
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
        struct Started: Decodable { let offset: Int; let chunkBytes: Int; let completed: Bool }
        struct Advanced: Decodable { let offset: Int }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let startedData = try await send(attachmentEndpoint("start", Start(transferId: id, name: name, bytes: data.count, sha256: hash)), operationID: id + "-start")
        let started = try decoder.decode(Started.self, from: startedData)
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
            let advancedData = try await send(attachmentEndpoint("chunk", Chunk(transferId: id, offset: offset, data: bytes)), operationID: id + "-chunk-\(offset)")
            let advanced = try decoder.decode(Advanced.self, from: advancedData)
            guard advanced.offset >= end, advanced.offset <= data.count else { throw AttachmentTransferError.invalidResponse }
            offset = advanced.offset
            progress(Double(offset) / Double(data.count))
        }
        try Task.checkCancellation()
        let response = try await send(attachmentEndpoint("finish", Finish(transferId: id)), operationID: id + "-finish")
        let stored = try decoder.decode(Stored.self, from: response)
        guard !stored.id.isEmpty, stored.bytes == data.count else { throw AttachmentTransferError.invalidResponse }
        return stored.id
    }


    /// One immutable download ID keeps a retry bound to the original file version.
    public func downloadAttachment(path: String, downloadID: UUID) async throws -> Data {
        struct Request: Encodable { let path: String; let offset: Int; let length: Int; let version: String? }
        struct Chunk: Decodable { let version: String; let data: String; let offset: Int; let total: Int; let nextOffset: Int; let eof: Bool }
        let encoder = JSONEncoder()
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        var output = Data()
        var version: String?
        var total: Int?
        while true {
            try Task.checkCancellation()
            let body = try encoder.encode(Request(path: path, offset: output.count, length: 65536, version: version))
            let response = try await send(Endpoint<Data>.data(.post, path: ["v1", "service", "files", "chunk"], body: body, contentType: "application/json"),
                operationID: downloadID.uuidString.lowercased() + "-download-\(output.count)")
            let chunk = try decoder.decode(Chunk.self, from: response)
            guard chunk.offset == output.count, (0...20 * 1024 * 1024).contains(chunk.total),
                  total == nil || total == chunk.total, !chunk.version.isEmpty,
                  version == nil || version == chunk.version,
                  !chunk.data.contains("="), chunk.data.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 })
            else { throw AttachmentTransferError.invalidResponse }
            let base64 = chunk.data.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            guard let bytes = Data(base64Encoded: base64 + String(repeating: "=", count: (4 - base64.count % 4) % 4)),
                  bytes.count <= 65536, chunk.nextOffset == output.count + bytes.count,
                  chunk.nextOffset <= chunk.total, chunk.eof == (chunk.nextOffset == chunk.total),
                  !bytes.isEmpty || chunk.eof else { throw AttachmentTransferError.invalidResponse }
            version = chunk.version; total = chunk.total
            output.append(bytes)
            if chunk.eof { return output }
        }
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
