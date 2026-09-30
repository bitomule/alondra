import AlondraAPIClient
import AlondraContracts
import AlondraViews
import CryptoKit
import Foundation

/// Transport boundary used by the feed and action actors. The API client implementation
/// preserves the view/action bytes and operation identity through loopback or relay.
public protocol ViewsTransport: Sendable {
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse
    func fetchContent(_ content: ContentRef) async throws -> String
    func fetchAttachment(_ attachment: AlondraViews.Attachment) async throws -> Data
}

public extension ViewsTransport {
    func fetchContent(_ content: ContentRef) async throws -> String { throw ViewsDataError.unsupportedContent }
    func fetchAttachment(_ attachment: AlondraViews.Attachment) async throws -> Data { throw ViewsDataError.unsupportedContent }
}

extension AlondraAPIClient: ViewsTransport {
    public func fetchView(key: String, client: ViewClient, lang: String, known: String? = nil) async throws -> ViewFetch {
        guard let route = ViewRoute.parse(key) else { throw ViewsDataError.invalidKey }
        let query: [(String, String)] = [("protocol", String(ViewsCodec.protocolVersion)), ("client", client.rawValue), ("lang", lang)]
            + route.query
            + (known.map { [("known", $0)] } ?? [])
        return try await send(Endpoint<ViewFetch>(.get, path: ["views", route.path], query: query) {
            try ViewsCodec.decode(ViewFetch.self, from: $0)
        })
    }

    public func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        precondition(wait <= 50)
        let viewList = views.map(\.feedValue).sorted().joined(separator: ",")
        let query: [(String, String)] = [("protocol", String(ViewsCodec.protocolVersion)), ("client", client.rawValue), ("lang", lang), ("wait", String(wait))]
            + (cursor.map { [("cursor", $0)] } ?? [])
            + (viewList.isEmpty ? [] : [("views", viewList)])
        return try await send(Endpoint<ViewFeed>(.get, path: ["changes"], query: query) {
            try ViewsCodec.decode(ViewFeed.self, from: $0)
        })
    }

    public func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        try await send(Endpoint<ActionResponse>(.post, path: ["actions"], body: body) {
            try ViewsCodec.decode(ActionResponse.self, from: $0)
        }, operationID: request.commandId)
    }

    public func fetchContent(_ content: ContentRef) async throws -> String {
        let path = content.fetch.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        guard content.fetch.hasPrefix("/"), path.allSatisfy(Self.validPathComponent) else {
            throw ViewsDataError.unsupportedContent
        }
        switch content.kind {
        case .document:
            guard path.count == 3, path[0] == "document-revisions", path[2] == "content",
                  content.fetch == "/document-revisions/\(path[1])/content", let digest = content.digest else {
                throw ViewsDataError.unsupportedContent
            }
            return try await readChunks(path: path, expectedDigest: digest, expectedID: path[1])
        case .diff:
            guard path.count == 3, path[0] == "pull-requests", path[2] == "diff",
                  content.fetch == "/pull-requests/\(path[1])/diff", let expectedHead = content.digest else {
                throw ViewsDataError.unsupportedContent
            }
            let diff = try await send(Endpoint<DiffResponse>(.get, path: path) { try JSONDecoder().decode(DiffResponse.self, from: $0) })
            guard diff.prID == path[1], diff.head == expectedHead else { throw ViewsDataError.invalidContent }
            return diff.diff
        case .file:
            guard path.first == "files", let digest = content.digest else { throw ViewsDataError.unsupportedContent }
            let data = try await readFileChunks(reference: content.fetch, digest: digest)
            guard let text = String(data: data, encoding: .utf8) else { throw ViewsDataError.invalidContent }
            return text
        }
    }

    public func fetchAttachment(_ attachment: AlondraViews.Attachment) async throws -> Data {
        guard let digest = attachment.digest, attachment.fetch.hasPrefix("/"), attachment.fetch.hasPrefix("/files/") else {
            throw ViewsDataError.unsupportedContent
        }
        return try await readFileChunks(reference: attachment.fetch, digest: digest)
    }

    private struct ContentChunk: Decodable {
        let revisionID: String; let contentDigest: String; let version: String; let data: String
        let offset: UInt64; let total: UInt64; let nextOffset: UInt64; let eof: Bool
        enum CodingKeys: String, CodingKey {
            case revisionID = "revision_id", contentDigest = "content_digest", version, data, offset, total, nextOffset = "next_offset", eof
        }
    }
    private struct FileChunk: Decodable {
        let contentDigest: String; let version: String; let data: String
        let offset: UInt64; let total: UInt64; let nextOffset: UInt64; let eof: Bool
        enum CodingKeys: String, CodingKey { case contentDigest = "content_digest", version, data, offset, total, nextOffset = "next_offset", eof }
    }
    private struct DiffResponse: Decodable {
        let prID: String; let head: String; let diff: String
        enum CodingKeys: String, CodingKey { case prID = "pr_id", head, diff }
    }

    private func readChunks(path: [String], expectedDigest: String, expectedID: String) async throws -> String {
        let bytes = try await readChunksData(path: path, expectedDigest: expectedDigest, expectedID: expectedID)
        guard let text = String(data: bytes, encoding: .utf8) else { throw ViewsDataError.invalidContent }
        return text
    }

    private func readChunksData(path: [String], expectedDigest: String, expectedID: String) async throws -> Data {
        let maximum = 32 * 1024 * 1024
        var collected = Data(); var version: String?; var total: UInt64?
        while true {
            let offset = collected.count
            let endpoint = Endpoint<ContentChunk>(.get, path: path,
                query: [("offset", String(offset))] + (version.map { [("version", $0)] } ?? [])) {
                    try JSONDecoder().decode(ContentChunk.self, from: $0)
                }
            let chunk = try await send(endpoint)
            guard chunk.revisionID == expectedID, chunk.contentDigest == expectedDigest,
                  Self.validDigest(chunk.contentDigest), !chunk.version.isEmpty,
                  version == nil || chunk.version == version, total == nil || chunk.total == total,
                  chunk.total <= UInt64(maximum), chunk.offset == UInt64(offset),
                  chunk.nextOffset <= chunk.total, chunk.nextOffset > chunk.offset || chunk.eof,
                  let decoded = Self.decodeBase64URL(chunk.data), UInt64(decoded.count) == chunk.nextOffset - chunk.offset else {
                throw ViewsDataError.invalidContent
            }
            collected.append(decoded); version = chunk.version; total = chunk.total
            if chunk.eof {
                guard chunk.nextOffset == chunk.total, Self.sha256(collected) == expectedDigest else { throw ViewsDataError.invalidContent }
                return collected
            }
        }
    }

    private func readFileChunks(reference: String, digest: String) async throws -> Data {
        let maximum = 32 * 1024 * 1024
        var collected = Data(); var version: String?; var total: UInt64?
        while true {
            let offset = collected.count
            let endpoint = Endpoint<FileChunk>(.get, path: ["files", "chunk"],
                query: [("path", reference), ("offset", String(offset))] + (version.map { [("version", $0)] } ?? [])) {
                    try JSONDecoder().decode(FileChunk.self, from: $0)
                }
            let chunk = try await send(endpoint)
            guard chunk.contentDigest == digest, Self.validDigest(digest), !chunk.version.isEmpty,
                  version == nil || chunk.version == version, total == nil || chunk.total == total,
                  chunk.total <= UInt64(maximum), chunk.offset == UInt64(offset), chunk.nextOffset <= chunk.total,
                  chunk.nextOffset > chunk.offset || chunk.eof,
                  let decoded = Self.decodeBase64URL(chunk.data), UInt64(decoded.count) == chunk.nextOffset - chunk.offset else {
                throw ViewsDataError.invalidContent
            }
            collected.append(decoded); version = chunk.version; total = chunk.total
            if chunk.eof {
                guard chunk.nextOffset == chunk.total, Self.sha256(collected) == digest else { throw ViewsDataError.invalidContent }
                return collected
            }
        }
    }

    private static func validPathComponent(_ value: String) -> Bool {
        !value.isEmpty && value != "." && value != ".." && value.utf8.count <= 256
            && value.utf8.allSatisfy { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95 }
    }
    private static func validDigest(_ digest: String) -> Bool {
        digest.utf8.count == 64 && digest.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    private static func decodeBase64URL(_ value: String) -> Data? {
        guard value.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }) else { return nil }
        let base64 = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        guard let data = Data(base64Encoded: base64 + String(repeating: "=", count: (4 - base64.count % 4) % 4)),
              data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") == value else { return nil }
        return data
    }
    private static func sha256(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
}

public enum ViewsDataError: Error, Equatable, Sendable {
    case invalidKey, invalidLanguage, missingView, unexpectedUnchanged, commandIdentityConflict, unknownCommand, unsupportedContent, invalidContent
}

struct ViewRoute {
    let path: String
    let query: [(String, String)]

    static func parse(_ key: String) -> ViewRoute? {
        guard !key.isEmpty, key.utf8.count <= 1024 else { return nil }
        let parts = key.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        let path = String(parts[0])
        if !["home", "sidebar", "tasks", "fleet", "schedules", "new_request", "issue_picker", "search", "machine"].contains(path) {
            let named = ["card", "document", "pull_request", "task", "request", "node", "schedule"]
            guard let colon = path.firstIndex(of: ":"), named.contains(String(path[..<colon])) else { return nil }
            let id = String(path[path.index(after: colon)...])
            guard validID(id) else { return nil }
        }
        guard parts.count == 2 else { return ViewRoute(path: path, query: []) }
        let raw = String(parts[1])
        guard !raw.isEmpty, let components = URLComponents(string: "alondra://view?" + raw.replacingOccurrences(of: "+", with: "%20")),
              let query = components.queryItems, !query.isEmpty else { return nil }
        var values: [String: String] = [:]
        for item in query {
            guard let value = item.value, values.updateValue(value, forKey: item.name) == nil,
                  value.utf8.count <= 256, !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { return nil }
        }
        switch path {
        case "home":
            guard values.count == 1, let focus = values["focus"], validID(focus) else { return nil }
            return ViewRoute(path: path, query: [("focus", focus)])
        case "tasks":
            guard values.count == 2, let section = values["section"], ["finished", "archived"].contains(section),
                  let after = values["after"], UInt(after) != nil else { return nil }
            return ViewRoute(path: path, query: [("section", section), ("after", after)])
        case "new_request", "schedule:new":
            guard values.count == 1, let project = values["project"] else { return nil }
            return ViewRoute(path: path, query: [("project", project)])
        case "issue_picker":
            if values == ["refresh": "1"] { return ViewRoute(path: path, query: [("refresh", "1")]) }
            guard values.count == 1, let q = values["q"] else { return nil }
            return ViewRoute(path: path, query: [("q", q)])
        case "search":
            guard values.count == 1, let q = values["q"] else { return nil }
            return ViewRoute(path: path, query: [("q", q)])
        default: return nil
        }
    }

    private static func validID(_ id: String) -> Bool {
        !id.isEmpty && id.utf8.count <= 256 && id.utf8.allSatisfy { byte in
            (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57)
                || byte == 45 || byte == 95
        }
    }
}

/// Canonical request preparation pins both the UUID and the encoded bytes before the first send.
/// Calling retry with the same command ID can only re-send that exact command.
public actor ViewActionSender {
    private struct Pending: Equatable, Sendable { let request: ActionRequest; let body: Data }
    private let transport: any ViewsTransport
    private var pending: [String: Pending] = [:]

    public init(transport: any ViewsTransport) { self.transport = transport }

    public func send(_ request: ActionRequest) async throws -> ActionResponse {
        let encoded = try ViewsCodec.encode(request)
        if let existing = pending[request.commandId], existing.body != encoded {
            throw ViewsDataError.commandIdentityConflict
        }
        pending[request.commandId] = Pending(request: request, body: encoded)
        return try await submit(pending[request.commandId]!)
    }

    public func retry(commandID: String) async throws -> ActionResponse {
        guard let value = pending[commandID] else { throw ViewsDataError.unknownCommand }
        return try await submit(value)
    }

    public func unresolvedCommandIDs() -> [String] { pending.keys.sorted() }

    private func submit(_ value: Pending) async throws -> ActionResponse {
        let response = try await transport.submitAction(value.request, body: value.body)
        pending.removeValue(forKey: value.request.commandId)
        return response
    }
}
