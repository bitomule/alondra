import AlondraAPIClient
import CryptoKit
import Foundation

enum LoopbackDigest {
    static func hex(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}

struct LoopbackUpload: Sendable {
    let name: String
    let bytes: Int
    let sha256: String
    var data = Data()
    var stored: Data?
}

struct LoopbackDownload: Sendable {
    let digest: String
    let data: Data
}

/// The app speaks the relay's chunked attachment protocol. Locally alondrad takes one raw
/// upload (`POST /v2/uploads?name=`) and serves a whole stored file (`GET /v2/files?path=`), so
/// this session buffers the chunks and slices the file itself.
extension LoopbackSession {
    private static let chunkBytes = 64 * 1024
    private static let maximumBytes = 20 * 1024 * 1024
    private static let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")

    func upload(_ operation: String, request: ServiceRequest) async throws -> LoopbackOutcome {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let body = request.body ?? Data()
        switch operation {
        case "start":
            struct Start: Decodable { let transferId: String; let name: String; let bytes: Int; let sha256: String }
            let start = try decoder.decode(Start.self, from: body)
            guard (1...Self.maximumBytes).contains(start.bytes) else { return Self.rejected(422, "invalid attachment size") }
            if let existing = uploads[start.transferId] {
                guard existing.sha256 == start.sha256, existing.bytes == start.bytes else {
                    return Self.rejected(409, "transfer identity reused for a different file")
                }
            } else {
                uploads[start.transferId] = LoopbackUpload(name: start.name, bytes: start.bytes, sha256: start.sha256)
            }
            let upload = uploads[start.transferId]!
            let offset = upload.stored == nil ? upload.data.count : upload.bytes
            return try Self.json(["offset": offset, "chunk_bytes": Self.chunkBytes, "completed": upload.stored != nil])
        case "chunk":
            struct Chunk: Decodable { let transferId: String; let offset: Int; let data: String }
            let chunk = try decoder.decode(Chunk.self, from: body)
            guard var upload = uploads[chunk.transferId], let bytes = Self.base64URL(chunk.data) else {
                return Self.rejected(404, "unknown transfer")
            }
            if chunk.offset == upload.data.count, upload.data.count + bytes.count <= upload.bytes {
                upload.data.append(bytes)
                uploads[chunk.transferId] = upload
            } else if chunk.offset + bytes.count > upload.data.count {
                return Self.rejected(409, "chunk out of order")
            }
            return try Self.json(["offset": upload.data.count])
        case "finish":
            struct Finish: Decodable { let transferId: String }
            let finish = try decoder.decode(Finish.self, from: body)
            guard var upload = uploads[finish.transferId] else { return Self.rejected(404, "unknown transfer") }
            if let stored = upload.stored { return .completed(status: 200, body: stored) }
            guard upload.data.count == upload.bytes, LoopbackDigest.hex(upload.data) == upload.sha256 else {
                return Self.rejected(422, "attachment bytes do not match")
            }
            let name = upload.name.addingPercentEncoding(withAllowedCharacters: Self.unreserved) ?? "attachment"
            let result = try await exchange(method: "POST", target: "/v2/uploads?name=\(name)", body: upload.data,
                                            operationID: request.operationID, contentType: "application/octet-stream")
            guard case let .completed(status, reference) = result, (200..<300).contains(status) else { return result }
            let attachment = try JSONSerialization.jsonObject(with: reference)
            let stored = try JSONSerialization.data(withJSONObject: ["attachment": attachment])
            upload.stored = stored
            upload.data = Data()
            uploads[finish.transferId] = upload
            return .completed(status: 200, body: stored)
        case "cancel":
            struct Cancel: Decodable { let transferId: String }
            uploads[try decoder.decode(Cancel.self, from: body).transferId] = nil
            return try Self.json(["cancelled": true])
        default:
            return Self.rejected(404, "unknown upload operation")
        }
    }

    /// `GET /v2/files/chunk?path=&offset=&version=`, answered from one whole-file read.
    func fileChunk(_ target: String) async throws -> LoopbackOutcome {
        let items = URLComponents(string: target)?.queryItems ?? []
        guard let path = items.first(where: { $0.name == "path" })?.value, !path.isEmpty,
              let offset = items.first(where: { $0.name == "offset" })?.value.flatMap(Int.init), offset >= 0 else {
            return Self.rejected(422, "invalid range")
        }
        let version = items.first { $0.name == "version" }?.value
        if downloads[path] == nil || offset == 0 {
            let encoded = path.addingPercentEncoding(withAllowedCharacters: Self.unreserved) ?? ""
            let result = try await exchange(method: "GET", target: "/v2/files?path=\(encoded)", body: nil, operationID: nil)
            guard case let .completed(status, data) = result, (200..<300).contains(status) else { return result }
            guard data.count <= Self.maximumBytes else { return Self.rejected(413, "file too large") }
            downloads[path] = LoopbackDownload(digest: LoopbackDigest.hex(data), data: data)
        }
        let file = downloads[path]!
        guard version == nil || version == file.digest else { return Self.rejected(409, "file changed while downloading") }
        guard offset <= file.data.count else { return Self.rejected(416, "offset past the end") }
        let end = min(offset + Self.chunkBytes, file.data.count)
        let slice = file.data.subdata(in: offset..<end)
        if end == file.data.count { downloads[path] = nil }
        let encoded = slice.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return try Self.json(["version": file.digest, "content_digest": file.digest, "data": encoded, "offset": offset,
                              "total": file.data.count, "next_offset": end, "eof": end == file.data.count])
    }

    private static func base64URL(_ text: String) -> Data? {
        let base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        return Data(base64Encoded: base64 + String(repeating: "=", count: (4 - base64.count % 4) % 4))
    }

    private static func json(_ object: [String: Any]) throws -> LoopbackOutcome {
        .completed(status: 200, body: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]))
    }

    private static func rejected(_ status: Int, _ message: String) -> LoopbackOutcome {
        .completed(status: status, body: (try? JSONSerialization.data(withJSONObject: ["code": "invalid_input", "message": message])) ?? Data())
    }
}
