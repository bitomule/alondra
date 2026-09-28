import AlondraAPIClient
import CryptoKit
import Foundation

struct LoopbackUpload: Sendable {
    let name: String
    let bytes: Int
    let sha256: String
    var data = Data()
    var stored: Data?
}

struct LoopbackDownload: Sendable {
    let version: String
    let data: Data
}

enum LoopbackDigest {
    static func hex(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}

/// The app speaks the relay's chunked attachment protocol. Locally alondrad takes one raw upload
/// and serves a whole file, so this session buffers the chunks and slices the file itself.
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
            let result = try await exchange(method: "POST", target: "/v1/uploads?name=\(name)", body: upload.data,
                                            operationID: request.operationID, contentType: "application/octet-stream")
            if case let .completed(status, stored) = result, (200..<300).contains(status) {
                upload.stored = stored
                upload.data = Data()
                uploads[finish.transferId] = upload
            }
            return result
        case "cancel":
            struct Cancel: Decodable { let transferId: String }
            uploads[try decoder.decode(Cancel.self, from: body).transferId] = nil
            return try Self.json(["cancelled": true])
        default:
            return Self.rejected(404, "unknown upload operation")
        }
    }

    func downloadChunk(_ body: Data) async throws -> LoopbackOutcome {
        struct Request: Decodable { let path: String; let offset: Int; let length: Int; let version: String? }
        let request = try JSONDecoder().decode(Request.self, from: body)
        guard request.offset >= 0, (1...Self.chunkBytes).contains(request.length) else {
            return Self.rejected(422, "invalid range")
        }
        if downloads[request.path] == nil || request.offset == 0 {
            let path = request.path.addingPercentEncoding(withAllowedCharacters: Self.unreserved) ?? ""
            let result = try await exchange(method: "GET", target: "/v1/file?path=\(path)", body: nil, operationID: nil)
            guard case let .completed(status, data) = result else { return result }
            guard (200..<300).contains(status) else { return result }
            guard data.count <= Self.maximumBytes else { return Self.rejected(413, "file too large") }
            downloads[request.path] = LoopbackDownload(version: LoopbackDigest.hex(data), data: data)
        }
        let file = downloads[request.path]!
        guard request.version == nil || request.version == file.version else {
            return Self.rejected(409, "file changed while downloading")
        }
        guard request.offset <= file.data.count else { return Self.rejected(416, "offset past the end") }
        let end = min(request.offset + request.length, file.data.count)
        let slice = file.data.subdata(in: request.offset..<end)
        if end == file.data.count { downloads[request.path] = nil }
        let encoded = slice.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return try Self.json(["version": file.version, "data": encoded, "offset": request.offset,
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
        .completed(status: status, body: (try? JSONSerialization.data(withJSONObject: ["error": message])) ?? Data())
    }
}
