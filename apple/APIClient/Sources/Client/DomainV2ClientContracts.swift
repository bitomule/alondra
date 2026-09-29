import AlondraContracts
import CryptoKit
import Foundation

/// The daemon's routes outside domain resources (docs/domain-v2-apple-client-contracts.md). The
/// TUI and CLI send the same bodies; fixtures/client-v2/requests.json holds each one.
extension AlondraAPIClient {
    public func domainRuntime() async throws -> DomainV2.Runtime {
        try await send(Endpoint<DomainV2.Runtime>(.get, path: ["v2", "runtime"]) { try DomainV2.Runtime.decode(from: $0) })
    }

    public func domainAgentActivity(_ agent: DomainV2.AgentId) async throws -> [NodeActivity] {
        try await send(Endpoint<NodeThreads>.json(.get, path: ["v2", "agents", agent.rawValue, "activity"])).activity ?? []
    }

    public func domainTaskHistory(_ task: DomainV2.TaskId) async throws -> DomainV2.TaskHistory {
        try await send(Endpoint<DomainV2.TaskHistory>.json(.get, path: ["v2", "tasks", task.rawValue, "history"]))
    }

    public func domainProjects() async throws -> [String] {
        try await send(Endpoint<ProjectList>.json(.get, path: ["v2", "projects"])).projects.map(\.name)
    }

    /// The daemon answers the options of the project asked for; the catalog records which one.
    public func domainOptions(project: String?) async throws -> OptionsCatalog {
        let served = try await send(Endpoint<OptionsCatalog>.json(.get, path: ["v2", "options"],
                                                                  query: project.map { [("project", $0)] } ?? []))
        return OptionsCatalog(project: project, options: served.options, error: served.error)
    }

    public func domainIssues(refresh: Bool) async throws -> IssueList {
        try await send(Endpoint<IssueList>.json(.get, path: ["v2", "issues"], query: refresh ? [("refresh", "1")] : []))
    }

    public func domainSearch(_ query: String) async throws -> SearchResult {
        try await send(Endpoint<SearchResult>.json(.get, path: ["v2", "agents", "search"], query: [("q", query)]))
    }

    /// Types the message into the agent's session. The kind tells the daemon how to word it.
    public func sendAgentMessage(_ agent: DomainV2.AgentId, _ message: AgentMessage, operationID: String) async throws -> AgentMessageReceipt {
        return try await send(Endpoint<AgentMessageReceipt>.json(.post, path: ["v2", "agents", agent.rawValue, "messages"],
                                                                 body: try DomainV2.encode(message)),
                              operationID: operationID)
    }

    /// The orchestrator is not a domain agent: its messages have a route of their own.
    public func sendOrchestratorMessage(_ message: AgentMessage, operationID: String) async throws -> AgentMessageReceipt {
        let body = try DomainV2.encode(OrchestratorMessage(text: message.text, attachments: message.attachments))
        return try await send(Endpoint<AgentMessageReceipt>.json(.post, path: ["v2", "orchestrator", "messages"], body: body),
                              operationID: operationID)
    }

    public func closeAgent(_ agent: DomainV2.AgentId, force: Bool, operationID: String) async throws {
        let body = try JSONSerialization.data(withJSONObject: ["force": force])
        _ = try await send(Endpoint<Data>.data(.post, path: ["v2", "agents", agent.rawValue, "close"], body: body,
                                               contentType: "application/json", requiresJSONObjectResponse: true),
                           operationID: operationID)
    }

    /// A request with its project, launch options, issue and attachments, created in one step.
    public func createRequest(_ request: NewRequestSubmission, operationID: String) async throws -> DomainV2.CommandOutcome {
        try await send(Endpoint<DomainV2.CommandOutcome>(.post, path: ["v2", "requests"], body: try DomainV2.encode(request),
                                                         contentType: "application/json", requiresJSONObjectResponse: true) {
            try DomainV2.decode(DomainV2.CommandOutcome.self, from: $0)
        }, operationID: operationID)
    }

    /// Attachments are immutable references with a content digest; every chunk is checked
    /// against it before any byte is returned.
    public func domainAttachmentContent(_ attachment: DomainV2.AttachmentRef) async throws -> Data {
        let maximum = 32 * 1024 * 1024
        var collected = Data()
        var version: String?
        var total: UInt64?
        repeat {
            let next = collected.count
            let endpoint = Endpoint<DomainV2.AttachmentContentChunk>(.get, path: ["v2", "files", "chunk"],
                query: [("path", attachment.contentReference), ("offset", String(next))] + (version.map { [("version", $0)] } ?? [])) {
                    try DomainV2.decode(DomainV2.AttachmentContentChunk.self, from: $0)
                }
            let chunk = try await send(endpoint)
            guard chunk.contentDigest == attachment.contentDigest,
                  !chunk.version.isEmpty, version == nil || chunk.version == version,
                  total == nil || chunk.total == total, chunk.total <= UInt64(maximum),
                  chunk.offset == UInt64(next), chunk.nextOffset <= chunk.total,
                  chunk.nextOffset > chunk.offset || chunk.eof,
                  let bytes = Self.canonicalBase64URL(chunk.data), UInt64(bytes.count) == chunk.nextOffset - chunk.offset
            else { throw Self.unavailable(attachment.id) }
            collected.append(bytes)
            version = chunk.version
            total = chunk.total
            if chunk.eof {
                guard chunk.nextOffset == chunk.total,
                      SHA256.hash(data: collected).map({ String(format: "%02x", $0) }).joined() == attachment.contentDigest
                else { throw Self.unavailable(attachment.id) }
                return collected
            }
        } while true
    }

    /// Resumable upload bound to `transferID`, the same protocol as the v1 service uploads.
    /// The finished upload is the immutable reference commands carry.
    public func uploadDomainAttachment(_ data: Data, name: String, transferID: UUID,
                                       progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> DomainV2.AttachmentRef {
        guard !data.isEmpty, data.count <= 20 * 1024 * 1024 else { throw AttachmentTransferError.invalidSize }
        guard !name.isEmpty, name.utf8.count <= 1024 else { throw AttachmentTransferError.invalidName }
        let id = transferID.uuidString.lowercased()
        let hash = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        struct Start: Encodable { let transferId: String; let name: String; let bytes: Int; let sha256: String }
        struct Chunk: Encodable { let transferId: String; let offset: Int; let data: String }
        struct Finish: Encodable { let transferId: String }
        struct Started: Decodable { let offset: Int; let chunkBytes: Int; let completed: Bool }
        struct Advanced: Decodable { let offset: Int }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let started = try decoder.decode(Started.self, from: try await send(
            uploadEndpoint("start", Start(transferId: id, name: name, bytes: data.count, sha256: hash)), operationID: id + "-start"))
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
                uploadEndpoint("chunk", Chunk(transferId: id, offset: offset, data: bytes)), operationID: id + "-chunk-\(offset)"))
            guard advanced.offset >= end, advanced.offset <= data.count else { throw AttachmentTransferError.invalidResponse }
            offset = advanced.offset
            progress(Double(offset) / Double(data.count))
        }
        try Task.checkCancellation()
        struct Finished: Decodable { let attachment: DomainV2.AttachmentRef }
        let stored = try DomainV2.decode(Finished.self, from: try await send(
            uploadEndpoint("finish", Finish(transferId: id)), operationID: id + "-finish")).attachment
        guard !stored.id.isEmpty, stored.bytes == UInt64(data.count), stored.contentDigest == hash, stored.name == name
        else { throw AttachmentTransferError.invalidResponse }
        return stored
    }

    public func cancelDomainUpload(_ transferID: UUID) async throws {
        struct Cancel: Encodable { let transferId: String }
        let id = transferID.uuidString.lowercased()
        _ = try await send(uploadEndpoint("cancel", Cancel(transferId: id)), operationID: id + "-cancel")
    }

    /// The user's decision on an interrupted orchestrator execution. The operation ID is the
    /// idempotency key: after an unknown outcome the same decision reuses it.
    public func decideOrchestratorRun(_ decision: OrchestratorDecision, runID: String, operationID: String) async throws {
        let body = try JSONSerialization.data(withJSONObject: ["run_id": runID])
        _ = try await send(Endpoint<Data>.data(.post, path: ["v2", "orchestrator", decision.rawValue], body: body,
                                               contentType: "application/json", requiresJSONObjectResponse: true),
                           operationID: operationID)
    }

    private func uploadEndpoint<Body: Encodable>(_ operation: String, _ body: Body) throws -> Endpoint<Data> {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return .data(.post, path: ["uploads", operation],
                     body: try encoder.encode(body), contentType: "application/json", requiresJSONObjectResponse: true)
    }

    static func canonicalBase64URL(_ text: String) -> Data? {
        guard text.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }) else { return nil }
        let base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        guard let bytes = Data(base64Encoded: base64 + String(repeating: "=", count: (4 - base64.count % 4) % 4)),
              bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") == text else { return nil }
        return bytes
    }

    private static func unavailable(_ id: String) -> DomainV2.DomainError {
        .init(code: "content_unavailable", message: "Attachment content changed or could not be verified", currentRevision: nil, subject: id)
    }
}

/// `POST /v2/orchestrator/{cancel,acknowledge,retry}` with the execution's `run_id`.
public enum OrchestratorDecision: String, Sendable {
    case cancel, acknowledge, retry
}

private struct OrchestratorMessage: Encodable {
    let text: String
    let attachments: [DomainV2.AttachmentRef]
}
