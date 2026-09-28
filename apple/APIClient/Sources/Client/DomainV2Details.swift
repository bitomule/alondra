import AlondraContracts
import CryptoKit
import Foundation

extension AlondraAPIClient {
    public func domainPullRequestDiff(_ id: DomainV2.PrId, expectedHead: String) async throws -> DomainV2.PullRequestDiff {
        let endpoint = Endpoint<DomainV2.PullRequestDiff>(.get,
            path: ["v2", "domain", "pull-requests", id.rawValue, "diff"]) {
                try DomainV2.decode(DomainV2.PullRequestDiff.self, from: $0)
            }
        let response = try await send(endpoint)
        guard response.prId == id, response.head == expectedHead else {
            throw DomainV2.DomainError(code: "stale_head", message: "The pull request head changed", currentRevision: nil, subject: id.rawValue)
        }
        return response
    }

    public func domainTaskActions(_ id: DomainV2.TaskId, epoch: DomainV2.DataEpoch) async throws -> DomainV2.TaskActions {
        let endpoint = Endpoint<DomainV2.TaskActions>(.get,
            path: ["v2", "domain", "tasks", id.rawValue, "actions"]) {
                try DomainV2.decode(DomainV2.TaskActions.self, from: $0)
            }
        let response = try await send(endpoint)
        guard response.protocolVersion == DomainV2.protocolVersion,
              response.dataEpoch == epoch, response.revision <= DomainV2.maximumRevision else {
            throw DomainV2.DomainError(code: "stale_epoch", message: "Task actions are from another workspace", currentRevision: response.revision, subject: id.rawValue)
        }
        return response
    }

    /// Fragmented content belongs to an immutable DocumentRevision. Every chunk must
    /// have the same metadata/version, canonical base64url bytes and exact offset.
    public func domainDocumentContent(_ id: DomainV2.DocumentRevisionId, expectedDigest: String) async throws -> String {
        let maximum = 32 * 1024 * 1024
        var collected = Data()
        var version: String?
        var total: UInt64?
        repeat {
            let next = collected.count
            let endpoint = Endpoint<DomainV2.DocumentContentChunk>(.get,
                path: ["v2", "domain", "document-revisions", id.rawValue, "content"],
                query: [("offset", String(next))] + (version.map { [("version", $0)] } ?? [])) {
                    try DomainV2.decode(DomainV2.DocumentContentChunk.self, from: $0)
                }
            let chunk = try await send(endpoint)
            guard chunk.revisionId == id, chunk.contentDigest == expectedDigest,
                  chunk.contentDigest.utf8.count == 64,
                  chunk.contentDigest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
                  !chunk.version.isEmpty, version == nil || chunk.version == version,
                  total == nil || chunk.total == total,
                  chunk.total <= UInt64(maximum), chunk.offset == UInt64(next),
                  chunk.nextOffset <= chunk.total, chunk.nextOffset > chunk.offset || chunk.eof
            else { throw invalidContent(id) }
            let base64 = chunk.data.replacingOccurrences(of: "-", with: "+")
                .replacingOccurrences(of: "_", with: "/")
            guard chunk.data.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }),
                  let bytes = Data(base64Encoded: base64 + String(repeating: "=", count: (4 - base64.count % 4) % 4)),
                  bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                    .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") == chunk.data,
                  UInt64(bytes.count) == chunk.nextOffset - chunk.offset
            else { throw invalidContent(id) }
            collected.append(bytes)
            version = chunk.version
            total = chunk.total
            if chunk.eof {
                guard chunk.nextOffset == chunk.total,
                      SHA256.hash(data: collected).map({ String(format: "%02x", $0) }).joined() == expectedDigest,
                      let text = String(data: collected, encoding: .utf8)
                else { throw invalidContent(id) }
                return text
            }
        } while true
    }

    private func invalidContent(_ id: DomainV2.DocumentRevisionId) -> DomainV2.DomainError {
        .init(code: "content_unavailable", message: "Document content changed or could not be verified", currentRevision: nil, subject: id.rawValue)
    }
}
