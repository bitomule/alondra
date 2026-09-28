import CryptoKit
import Foundation

/// Bounded, all-or-nothing assembly. No server-sized allocation before validating a chunk.
struct OverviewSnapshotAssembly {
    struct Chunk: Decodable {
        let schema: Int
        let snapshot_id: String
        let total_bytes: Int
        let sha256: String
        let chunk_bytes: Int
        let offset: Int
        let data: String
    }
    static let root = "/v1/service/overview-snapshot"
    static let maximum = 8 * 1024 * 1024
    let snapshotRoot: String
    let first: Chunk
    private(set) var bytes = Data()
    var path: String { snapshotRoot + "/" + first.snapshot_id }
    var complete: Bool { bytes.count == first.total_bytes }

    init(_ body: Data, root: String = Self.root) throws {
        guard [Self.root, "/v2/domain/snapshot"].contains(root) else { throw RelaySessionError.invalidRequest }
        snapshotRoot = root
        guard body.count <= 100_000 else { throw RelaySessionError.invalidResponse }
        let chunk = try JSONDecoder().decode(Chunk.self, from: body)
        guard Self.hex(chunk.snapshot_id, count: 32), Self.hex(chunk.sha256, count: 64),
              chunk.schema == 1, chunk.chunk_bytes == 65_536,
              (1...Self.maximum).contains(chunk.total_bytes) else { throw RelaySessionError.invalidResponse }
        first = chunk
        try append(body)
    }

    mutating func append(_ body: Data) throws {
        guard body.count <= 100_000 else { throw RelaySessionError.invalidResponse }
        let chunk = try JSONDecoder().decode(Chunk.self, from: body)
        guard chunk.schema == first.schema, chunk.snapshot_id == first.snapshot_id,
              chunk.total_bytes == first.total_bytes, chunk.sha256 == first.sha256,
              chunk.chunk_bytes == first.chunk_bytes, chunk.offset == bytes.count,
              chunk.data.utf8.count <= 87_382,
              chunk.data.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 })
        else { throw RelaySessionError.invalidResponse }
        let encoded = chunk.data.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        guard let decoded = Data(base64Encoded: encoded + String(repeating: "=", count: (4 - encoded.count % 4) % 4)),
              decoded.count == min(65_536, first.total_bytes - bytes.count),
              decoded.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") == chunk.data
        else { throw RelaySessionError.invalidResponse }
        bytes.append(decoded)
    }

    func finish() throws -> Data {
        guard complete, SHA256.hash(data: bytes).map({ String(format: "%02x", $0) }).joined() == first.sha256 else {
            throw RelaySessionError.invalidResponse
        }
        return bytes
    }

    static func hex(_ text: String, count: Int) -> Bool {
        text.utf8.count == count && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
}
