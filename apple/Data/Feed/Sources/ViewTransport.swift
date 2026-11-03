import AlondraTransport
import AlondraContract
import CryptoKit
import Foundation
import Synchronization

/// Transport boundary used by the feed and action actors. The API client implementation
/// preserves the view/action bytes and operation identity through loopback or relay.
public protocol ViewTransport: Sendable {
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed
    func fetchView(key: String, client: ViewClient, lang: String, known: String?, capabilities: [String]) async throws -> ViewFetch
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion], capabilities: [String]) async throws -> ViewFeed
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState?
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws
    func recoverAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse?
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse
    /// What the Mac holds for an action already sent, telling apart "nothing finished" from no answer.
    func lookupAction(_ request: ActionRequest, body: Data) async throws -> Recorded<ActionResponse>
    /// Sends an action again with the sealed bytes it already went out with, or seals it now if it never left.
    func resendAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse?
    func fetchContent(_ content: ContentRef) async throws -> String
    func fetchDiff(_ content: ContentRef) async throws -> FetchedDiff
    /// Ticks or clears GitHub's «Viewed» box of one file, for the head the reader saw.
    func setFileViewed(_ viewed: Bool, path: String, marks: FileMarks) async throws -> FileViewed
    func fetchAttachment(_ attachment: AlondraContract.Attachment) async throws -> Data
    /// Writes a card attachment to `file` block by block, verified against its digest before it returns.
    func downloadAttachment(_ attachment: AlondraContract.Attachment, to file: URL,
                            progress: @escaping @Sendable (UInt64, UInt64) -> Void) async throws
}

public extension ViewTransport {
    func fetchView(key: String, client: ViewClient, lang: String, known: String?, capabilities: [String]) async throws -> ViewFetch {
        try await fetchView(key: key, client: client, lang: lang, known: known)
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion], capabilities: [String]) async throws -> ViewFeed {
        try await fetchChanges(client: client, lang: lang, wait: wait, cursor: cursor, views: views)
    }
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState? { nil }
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws { throw OperationError.invalidEndpoint }
    func recoverAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? { nil }
    func lookupAction(_ request: ActionRequest, body: Data) async throws -> Recorded<ActionResponse> {
        guard let response = try await recoverAction(request, body: body) else { return .unknown }
        return .answered(response)
    }
    func resendAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? { nil }
    func fetchContent(_ content: ContentRef) async throws -> String { throw FeedError.unsupportedContent }
    func fetchDiff(_ content: ContentRef) async throws -> FetchedDiff { throw FeedError.unsupportedContent }
    func setFileViewed(_ viewed: Bool, path: String, marks: FileMarks) async throws -> FileViewed { throw FeedError.unsupportedContent }
    func fetchAttachment(_ attachment: AlondraContract.Attachment) async throws -> Data { throw FeedError.unsupportedContent }
    func downloadAttachment(_ attachment: AlondraContract.Attachment, to file: URL,
                            progress: @escaping @Sendable (UInt64, UInt64) -> Void) async throws { throw FeedError.unsupportedContent }
}

extension AlondraAPIClient: ViewTransport {
    public func fetchView(key: String, client: ViewClient, lang: String, known: String? = nil) async throws -> ViewFetch {
        try await fetchView(key: key, client: client, lang: lang, known: known, capabilities: [])
    }

    public func fetchView(key: String, client: ViewClient, lang: String, known: String?, capabilities: [String]) async throws -> ViewFetch {
        guard let route = ViewRoute.parse(key) else { throw FeedError.invalidKey }
        let query: [(String, String)] = [("protocol", String(ContractCodec.protocolVersion)), ("client", client.rawValue), ("lang", lang)]
            + route.query
            + (known.map { [("known", $0)] } ?? [])
            + capabilityQuery(capabilities)
        return try await send(Endpoint<ViewFetch>(.get, path: ["views", route.path], query: query) {
            try ContractCodec.decode(ViewFetch.self, from: $0)
        })
    }

    public func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        try await fetchChanges(client: client, lang: lang, wait: wait, cursor: cursor, views: views, capabilities: [])
    }

    public func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion], capabilities: [String]) async throws -> ViewFeed {
        precondition(wait <= 50)
        let viewList = views.map(\.feedValue).sorted().joined(separator: ",")
        let query: [(String, String)] = [("protocol", String(ContractCodec.protocolVersion)), ("client", client.rawValue), ("lang", lang), ("wait", String(wait))]
            + (cursor.map { [("cursor", $0)] } ?? [])
            + (viewList.isEmpty ? [] : [("views", viewList)])
            + capabilityQuery(capabilities)
        return try await send(Endpoint<ViewFeed>(.get, path: ["changes"], query: query) {
            try ContractCodec.decode(ViewFeed.self, from: $0)
        })
    }

    private func capabilityQuery(_ capabilities: [String]) -> [(String, String)] {
        let value = Set(capabilities).sorted().joined(separator: ",")
        return value.isEmpty ? [] : [("capabilities", value)]
    }

    public func recoverAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? {
        try await recover(Endpoint<ActionResponse>(.post, path: ["actions"], body: body) {
            try ContractCodec.decode(ActionResponse.self, from: $0)
        }, operationID: request.commandId)
    }

    public func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        try await send(Endpoint<ActionResponse>(.post, path: ["actions"], body: body) {
            try ContractCodec.decode(ActionResponse.self, from: $0)
        }, operationID: request.commandId)
    }

    public func lookupAction(_ request: ActionRequest, body: Data) async throws -> Recorded<ActionResponse> {
        try await lookup(Endpoint<ActionResponse>(.post, path: ["actions"], body: body) {
            try ContractCodec.decode(ActionResponse.self, from: $0)
        }, operationID: request.commandId)
    }

    public func resendAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? {
        try await resend(Endpoint<ActionResponse>(.post, path: ["actions"], body: body) {
            try ContractCodec.decode(ActionResponse.self, from: $0)
        }, operationID: request.commandId)
    }

    public func fetchContent(_ content: ContentRef) async throws -> String {
        let path = content.fetch.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        let ordinaryPath = path.allSatisfy(Self.validPathComponent)
        let encodedDiffPath = content.kind == .diff && path.count == 3
            && Self.validPathComponent(path[0]) && Self.validPathComponent(path[2])
        guard content.fetch.hasPrefix("/"), ordinaryPath || encodedDiffPath else {
            throw FeedError.unsupportedContent
        }
        switch content.kind {
        case .document:
            guard path.count == 3, path[0] == "document-revisions", path[2] == "content",
                  content.fetch == "/document-revisions/\(path[1])/content", let digest = content.digest else {
                throw FeedError.unsupportedContent
            }
            return try await readChunks(path: path, expectedDigest: digest, expectedID: path[1])
        case .diff:
            return try await fetchDiff(content).text
        case .file:
            guard path.first == "files", let digest = content.digest else { throw FeedError.unsupportedContent }
            let data = try await readFileChunks(reference: content.fetch, digest: digest)
            guard let text = String(data: data, encoding: .utf8) else { throw FeedError.invalidContent }
            return text
        }
    }

    public func fetchAttachment(_ attachment: AlondraContract.Attachment) async throws -> Data {
        if let route = AttachmentRoute(attachment) {
            var collected = Data()
            try await readAttachment(route, consume: { collected.append($0) }, progress: { _, _ in })
            return collected
        }
        guard let digest = attachment.digest, attachment.fetch.hasPrefix("files/"),
              !attachment.fetch.split(separator: "/").contains("..") else {
            throw FeedError.unsupportedContent
        }
        return try await readFileChunks(reference: attachment.fetch, digest: digest)
    }

    public func downloadAttachment(_ attachment: AlondraContract.Attachment, to file: URL,
                                   progress: @escaping @Sendable (UInt64, UInt64) -> Void) async throws {
        guard let route = AttachmentRoute(attachment) else {
            let data = try await fetchAttachment(attachment)
            try data.write(to: file, options: .atomic)
            progress(UInt64(data.count), UInt64(data.count))
            return
        }
        guard FileManager.default.createFile(atPath: file.path, contents: nil) else { throw FeedError.invalidContent }
        let handle = try FileHandle(forWritingTo: file)
        defer { try? handle.close() }
        try await readAttachment(route, consume: { try handle.write(contentsOf: $0) }, progress: progress)
    }

    private struct AttachmentChunk: Decodable {
        let interactionID: String; let attachmentID: String; let contentDigest: String; let version: String; let data: String
        let offset: UInt64; let total: UInt64; let nextOffset: UInt64; let eof: Bool
        enum CodingKeys: String, CodingKey {
            case interactionID = "interaction_id", attachmentID = "attachment_id", contentDigest = "content_digest", version, data
            case offset, total, nextOffset = "next_offset", eof
        }
    }

    private func readAttachment(_ route: AttachmentRoute, consume: (Data) throws -> Void,
                                progress: @Sendable (UInt64, UInt64) -> Void) async throws {
        let maximum = UInt64(APIRequest.attachmentContentLimit)
        var hasher = SHA256(); var offset: UInt64 = 0; var version: String?; var total: UInt64?
        while true {
            try Task.checkCancellation()
            let endpoint = Endpoint<AttachmentChunk>(.get, path: ["interactions", route.interaction, "attachments", route.attachment, "content"],
                query: [("offset", String(offset))] + (version.map { [("version", $0)] } ?? [])) {
                    try JSONDecoder().decode(AttachmentChunk.self, from: $0)
                }
            let chunk = try await send(endpoint)
            guard chunk.interactionID == route.interaction, chunk.attachmentID == route.attachment,
                  chunk.contentDigest == route.digest, !chunk.version.isEmpty, chunk.version.utf8.count <= 128,
                  version == nil || chunk.version == version, total == nil || chunk.total == total,
                  chunk.total <= maximum, chunk.offset == offset, chunk.nextOffset <= chunk.total,
                  chunk.nextOffset > chunk.offset || chunk.eof,
                  let decoded = Self.decodeBase64URL(chunk.data), UInt64(decoded.count) == chunk.nextOffset - chunk.offset else {
                throw FeedError.invalidContent
            }
            hasher.update(data: decoded)
            try consume(decoded)
            offset = chunk.nextOffset; version = chunk.version; total = chunk.total
            progress(offset, chunk.total)
            if chunk.eof {
                let digest = hasher.finalize().map { String(format: "%02x", $0) }.joined()
                guard chunk.nextOffset == chunk.total, digest == route.digest else { throw FeedError.invalidContent }
                return
            }
        }
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
        let prID: String; let head: String; let diff: String; let truncated: Bool?; let files: [Mark]?
        enum CodingKeys: String, CodingKey { case prID = "pr_id", head, diff, truncated, files }
    }
    private struct Mark: Decodable {
        let path: String; let viewed: String
    }
    private struct MarkResponse: Decodable {
        let prID: String; let head: String; let path: String; let viewed: FileViewed
        enum CodingKeys: String, CodingKey { case prID = "pr_id", head, path, viewed }
    }

    public func fetchDiff(_ content: ContentRef) async throws -> FetchedDiff {
        let path = content.fetch.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        guard content.kind == .diff, content.fetch.hasPrefix("/"), path.count == 3, path[0] == "pull-requests", path[2] == "diff",
              Self.validPathComponent(path[0]), Self.validPathComponent(path[2]),
              let rawID = Self.decodedPullRequestID(from: content.fetch), let expectedHead = content.digest else {
            throw FeedError.unsupportedContent
        }
        let diff = try await send(Endpoint<DiffResponse>(.get, path: ["pull-requests", rawID, "diff"]) {
            try JSONDecoder().decode(DiffResponse.self, from: $0)
        })
        guard diff.prID == rawID, diff.head == expectedHead else { throw FeedError.invalidContent }
        let marks = diff.files.map { files in
            FileMarks(prID: rawID, head: diff.head, files: Dictionary(files.compactMap { mark in
                FileViewed(rawValue: mark.viewed).map { (mark.path, $0) }
            }, uniquingKeysWith: { first, _ in first }))
        }
        return FetchedDiff(text: diff.diff, truncated: diff.truncated ?? false, marks: marks)
    }

    public func setFileViewed(_ viewed: Bool, path: String, marks: FileMarks) async throws -> FileViewed {
        let body = try JSONSerialization.data(withJSONObject: ["path": path, "viewed": viewed, "head": marks.head], options: [.sortedKeys])
        let endpoint = Endpoint<MarkResponse>(.post, path: ["pull-requests", marks.prID, "viewed-files"], body: body,
                                              requiresJSONObjectResponse: true) {
            try JSONDecoder().decode(MarkResponse.self, from: $0)
        }
        let response: MarkResponse
        do { response = try await send(endpoint, operationID: UUID().uuidString) }
        catch APIError.conflict { throw FileMarkError.stale }
        catch APIError.server(let status, _, _, _) where status == 404 || status == 422 { throw FileMarkError.refused }
        guard response.prID == marks.prID, response.head == marks.head, response.path == path else { throw FeedError.invalidContent }
        return response.viewed
    }

    private func readChunks(path: [String], expectedDigest: String, expectedID: String) async throws -> String {
        let bytes = try await readChunksData(path: path, expectedDigest: expectedDigest, expectedID: expectedID)
        guard let text = String(data: bytes, encoding: .utf8) else { throw FeedError.invalidContent }
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
                throw FeedError.invalidContent
            }
            collected.append(decoded); version = chunk.version; total = chunk.total
            if chunk.eof {
                guard chunk.nextOffset == chunk.total, Self.sha256(collected) == expectedDigest else { throw FeedError.invalidContent }
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
                throw FeedError.invalidContent
            }
            collected.append(decoded); version = chunk.version; total = chunk.total
            if chunk.eof {
                guard chunk.nextOffset == chunk.total, Self.sha256(collected) == digest else { throw FeedError.invalidContent }
                return collected
            }
        }
    }

    private static func validPathComponent(_ value: String) -> Bool {
        !value.isEmpty && value != "." && value != ".." && value.utf8.count <= 256
            && value.utf8.allSatisfy { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95 }
    }
    static func decodedPullRequestID(from fetch: String) -> String? {
        let parts = fetch.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 4, parts[0].isEmpty, parts[1] == "pull-requests", parts[3] == "diff",
              let decoded = String(parts[2]).removingPercentEncoding,
              APIRequest.validViewID(decoded) else { return nil }
        let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        guard let canonical = decoded.addingPercentEncoding(withAllowedCharacters: unreserved),
              canonical == parts[2] else { return nil }
        return decoded
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

public enum FeedError: Error, Equatable, Sendable {
    case invalidKey, invalidLanguage, missingView, unexpectedUnchanged, commandIdentityConflict, unknownCommand, unsupportedContent, invalidContent
}

public extension FeedError {
    /// A download that can never succeed by trying again: the Mac no longer holds that file as the card
    /// names it, or what it sent does not match the card. Anything else is the connection.
    static func attachmentIsGone(_ error: Error) -> Bool {
        switch error {
        case FeedError.invalidContent, FeedError.unsupportedContent: true
        case APIError.conflict, APIError.forbidden: true
        case let APIError.server(status, _, _, _): status == 404 || status == 409 || status == 422
        case let APIError.unexpected(status): status == 404 || status == 409
        default: false
        }
    }
}

/// A card attachment as the daemon serves it: `/interactions/{interaction}/attachments/{attachment}/content`,
/// each id percent-encoded once with the daemon's own alphabet.
struct AttachmentRoute: Equatable {
    let interaction: String
    let attachment: String
    let digest: String

    init?(_ attachment: AlondraContract.Attachment) {
        guard let digest = attachment.digest, digest.utf8.count == 64,
              digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              !attachment.fetch.contains("?"), !attachment.fetch.contains("#") else { return nil }
        let parts = attachment.fetch.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        guard parts.count == 6, parts[0].isEmpty, parts[1] == "interactions", parts[3] == "attachments", parts[5] == "content",
              let interaction = Self.decoded(parts[2]), let id = Self.decoded(parts[4]), id == attachment.id,
              APIRequest.validClientContractRoute(method: .get, path: ["interactions", interaction, "attachments", id, "content"])
        else { return nil }
        self.interaction = interaction
        self.attachment = id
        self.digest = digest
    }

    private static func decoded(_ raw: String) -> String? {
        guard let value = raw.removingPercentEncoding, !value.contains("~") else { return nil }
        let plain = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._")
        guard value.addingPercentEncoding(withAllowedCharacters: plain) == raw else { return nil }
        return value
    }
}

struct ViewRoute {
    let path: String
    let query: [(String, String)]

    static func parse(_ key: String) -> ViewRoute? {
        guard !key.isEmpty, key.utf8.count <= 1024 else { return nil }
        let parts = key.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        let path = String(parts[0])
        if !["home", "sidebar", "tasks", "schedules", "new_request", "issue_picker", "search", "machine", "mcp_servers", "accounts"].contains(path) {
            let named = ["card", "document", "pull_request", "task", "request", "node", "schedule"]
            guard let colon = path.firstIndex(of: ":"), named.contains(String(path[..<colon])) else { return nil }
            let id = String(path[path.index(after: colon)...])
            guard APIRequest.validViewID(id) else { return nil }
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
            guard values.count == 1, let focus = values["focus"], APIRequest.validViewID(focus) else { return nil }
            return ViewRoute(path: path, query: [("focus", focus)])
        case "tasks":
            guard values.count == 2, let section = values["section"], ["active", "finished"].contains(section),
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
}

/// Canonical intent is stored before its first dispatch. An unresolved intent is recovered by GET,
/// including when a new screen supplies another UUID for the same action after relaunch.
public actor ViewActionSender {
    private struct Pending: Equatable, Sendable { let request: ActionRequest; let body: Data }
    private let transport: any ViewTransport
    private let intents: ViewActionIntentStore?
    private let retryPolicy: ActionRetryPolicy
    private let signal = RetrySignal()
    public nonisolated let undelivered = UndeliveredCards()
    public nonisolated let setAside: SetAsideNotice
    private var pending: [String: Pending] { didSet { publishUndelivered() } }
    private var deliveries: [String: Task<ActionResponse, any Error>] = [:]
    private var activeEpoch: String?
    private var stopped = false
    private var retirementDeferred = false
    private var retirementRequested = false
    private var retirementTask: Task<Void, Never>?
    private var lastRetirement: ActionJournalAcknowledgment?
    private var journalEpoch: String?
    private var journalDescriptor: ActionJournalState?

    public init(transport: any ViewTransport, intents: ViewActionIntentStore? = nil, setAside: SetAsideNotice = SetAsideNotice(),
                retryPolicy: ActionRetryPolicy = .live) {
        self.transport = transport; self.intents = intents; self.setAside = setAside; self.retryPolicy = retryPolicy
        pending = Dictionary(uniqueKeysWithValues: (intents?.restored ?? []).map {
            ($0.request.commandId, Pending(request: $0.restoredRequest, body: $0.body))
        })
        undelivered.update(Set(pending.values.compactMap(\.request.answeredCard)))
    }

    private func journalState(dataEpoch: String) async throws -> ActionJournalState? {
        if journalEpoch == dataEpoch { return journalDescriptor }
        let descriptor = try await transport.actionJournal(dataEpoch: dataEpoch)
        try Task.checkCancellation()
        journalEpoch = dataEpoch; journalDescriptor = descriptor
        return descriptor
    }

    /// The Mac's journal numbers every answer, so an answer cannot be numbered before it is known
    /// once; until then it waits for the network like any undelivered answer.
    private func reachableJournalState(dataEpoch: String) async throws -> ActionJournalState? {
        var attempt = 0
        while true {
            let wakes = signal.generation
            do { return try await journalState(dataEpoch: dataEpoch) }
            catch where ActionDeliveryDisposition(error) == .retry && !(error is CancellationError) {}
            try checkDelivering()
            if wakes != signal.generation { attempt = 0; continue }
            attempt += 1
            await pause(attempt, since: wakes)
        }
    }

    public func prepare(_ request: ActionRequest) async throws -> ActionRequest {
        try Task.checkCancellation()
        guard let intents else { return request }
        if ActionOperationID(rawValue: request.commandId) != nil { return request }
        let descriptor = try await reachableJournalState(dataEpoch: request.dataEpoch)
        try Task.checkCancellation()
        guard let descriptor else {
            let history = try await intents.acknowledgmentRanges()
            guard history.generation == 0 else { throw OperationError.outcomeUnknown }
            return request
        }
        let record: ViewActionIntentStore.Record
        do { record = try await intents.prepareNew(request, generation: descriptor.generation) }
        catch ViewActionIntentError.capacityExceeded {
            scheduleRetirement()
            await drainRetirement()
            try Task.checkCancellation()
            record = try await intents.prepareNew(request, generation: descriptor.generation)
        }
        pending = pending.filter { _, value in
            guard let identity = ActionOperationID(rawValue: value.request.commandId) else { return true }
            return identity.generation >= descriptor.generation
        }
        try Task.checkCancellation()
        pending[record.request.commandId] = Pending(request: record.restoredRequest, body: record.body)
        scheduleRetirement()
        return record.restoredRequest
    }

    public func retirementNeedsRetry() -> Bool { retirementDeferred }

    public func send(_ request: ActionRequest) async throws -> ActionResponse {
        try Task.checkCancellation()
        let encoded = try ContractCodec.encode(request)
        if let intents {
            var allowLegacy = false
            if ActionOperationID(rawValue: request.commandId) == nil {
                let descriptor = try await journalState(dataEpoch: request.dataEpoch)
                let history = try await intents.acknowledgmentRanges()
                allowLegacy = descriptor == nil && history.generation == 0
            }
            switch try await intents.reserve(request, body: encoded, allowLegacyDispatch: allowLegacy) {
            case let .completed(response):
                if let response { return response }
                return try await recover(Pending(request: request, body: encoded))
            case let .pending(record):
                let value = Pending(request: record.restoredRequest, body: record.body)
                pending[value.request.commandId] = value
                return try await recover(value)
            case let .created(record):
                let value = Pending(request: record.restoredRequest, body: record.body)
                pending[value.request.commandId] = value
                try Task.checkCancellation()
                let response = try await transport.submitAction(value.request, body: value.body)
                try await applied(value, response: response)
                return response
            }
        }
        if let existing = pending[request.commandId] {
            guard existing.body == encoded else { throw FeedError.commandIdentityConflict }
            return try await recover(existing)
        }
        if let existing = pending.keys.sorted().compactMap({ pending[$0] }).first(where: {
            ViewActionIntentStore.sameIntent($0.request, request)
        }) { return try await recover(existing) }
        let value = Pending(request: request, body: encoded)
        pending[request.commandId] = value
        let response = try await transport.submitAction(request, body: encoded)
        try await applied(value, response: response)
        return response
    }

    /// An answer given where nothing can wait for the network, such as a notification button: one
    /// try. When the Mac cannot be asked, it is numbered from the journal generation this device
    /// last knew, kept, and throws `AnswerKeptForLater`; it then goes on its own once the Mac answers.
    /// Without such a generation it throws `AnswerNotSent`.
    public func sendNow(_ request: ActionRequest) async throws -> ActionResponse {
        try Task.checkCancellation()
        guard let intents, ActionOperationID(rawValue: request.commandId) == nil else { return try await send(request) }
        let generation: UInt64
        do {
            guard let descriptor = try await journalState(dataEpoch: request.dataEpoch) else { return try await send(request) }
            generation = descriptor.generation
        } catch where ActionDeliveryDisposition(error) == .retry && !(error is CancellationError) {
            let known = try await intents.acknowledgmentRanges()
            guard known.generation > 0, known.dataEpoch == request.dataEpoch else { throw AnswerNotSent() }
            generation = known.generation
        }
        let record = try await intents.prepareNew(request, generation: generation)
        var current = Pending(request: record.restoredRequest, body: record.body)
        pending[current.request.commandId] = current
        scheduleRetirement()
        do {
            return try await attemptDelivery(&current)
        } catch where ActionDeliveryDisposition(error) == .final {
            await discard(current)
            throw error
        } catch {
            throw AnswerKeptForLater()
        }
    }

    /// Delivers an answer whatever the network does: it goes again on its own, waiting longer each
    /// time and at once on `retryNow`, until the Mac answers it. Throws only when it can never be
    /// delivered. The delivery goes on when the caller stops waiting for it.
    public func deliver(_ request: ActionRequest) async throws -> ActionResponse {
        try Task.checkCancellation()
        let key = request.deliveryCorrelationID
        if let running = deliveries[key] { return try await running.value }
        let task = try startDelivery(Pending(request: request, body: ContractCodec.encode(request)))
        return try await task.value
    }

    /// The network came back, the app came to the front or a push arrived: every waiting answer goes now.
    public func retryNow() {
        signal.fire()
    }

    /// Answers left from an earlier launch go on by themselves.
    public func resumeDeliveries(dataEpoch: String) {
        activeEpoch = dataEpoch
        publishUndelivered()
        for value in pending.keys.sorted().compactMap({ pending[$0] }) where value.request.dataEpoch == dataEpoch {
            _ = try? startDelivery(value)
        }
    }

    public func stop() {
        stopped = true
        for task in deliveries.values { task.cancel() }
        deliveries.removeAll()
        signal.fire()
    }

    // Tests wait for deliveries started without a caller.
    func drainDeliveries() async {
        while let task = deliveries.values.first { _ = try? await task.value }
    }

    @discardableResult
    private func startDelivery(_ value: Pending) throws -> Task<ActionResponse, any Error> {
        guard !stopped else { throw CancellationError() }
        let key = value.request.deliveryCorrelationID
        if let running = deliveries[key] { return running }
        let task = Task { try await self.keepDelivering(value, key: key) }
        deliveries[key] = task
        return task
    }

    private func keepDelivering(_ start: Pending, key: String) async throws -> ActionResponse {
        defer { deliveries[key] = nil }
        var current = start
        var attempt = 0
        while true {
            try checkDelivering()
            let wakes = signal.generation
            do {
                return try await attemptDelivery(&current)
            } catch where ActionDeliveryDisposition(error) == .final {
                await discard(current)
                throw error
            } catch {}
            try checkDelivering()
            if let activeEpoch, activeEpoch != current.request.dataEpoch {
                await discard(current)
                throw ActionDeliveryError.dataReset
            }
            if wakes != signal.generation {
                attempt = 0
                continue
            }
            attempt += 1
            await pause(attempt, since: wakes)
        }
    }

    private func checkDelivering() throws {
        if stopped || Task.isCancelled { throw CancellationError() }
    }

    private func pause(_ attempt: Int, since wakes: Int) async {
        let delay = retryPolicy.delay(attempt), sleep = retryPolicy.sleep, signal = signal
        await withTaskGroup(of: Void.self) { group in
            group.addTask { try? await sleep(delay) }
            group.addTask { await signal.wait(since: wakes) }
            await group.next()
            group.cancelAll()
        }
    }

    /// One try: the first dispatch, or for an answer already sent, what the Mac holds and else the
    /// same sealed bytes again. Bytes too old to be accepted become the same answer under a new ID,
    /// only once the Mac said it never finished the old one.
    private func attemptDelivery(_ current: inout Pending) async throws -> ActionResponse {
        let request = current.request, encoded = current.body
        guard let intents else {
            if let existing = pending[request.commandId] {
                guard existing.body == encoded else { throw FeedError.commandIdentityConflict }
                return try await recoverOrResend(&current)
            }
            pending[request.commandId] = current
            let response = try await transport.submitAction(request, body: encoded)
            try await applied(current, response: response)
            return response
        }
        var allowLegacy = false
        if ActionOperationID(rawValue: request.commandId) == nil {
            let descriptor = try await journalState(dataEpoch: request.dataEpoch)
            let history = try await intents.acknowledgmentRanges()
            allowLegacy = descriptor == nil && history.generation == 0
        }
        switch try await intents.reserve(request, body: encoded, allowLegacyDispatch: allowLegacy) {
        case let .completed(response):
            if let response { return response }
            switch try await transport.lookupAction(request, body: encoded) {
            case let .answered(response):
                try await applied(current, response: response)
                return response
            case .absent: throw ActionDeliveryError.answeredBefore
            case .unknown: throw OperationError.outcomeUnknown
            }
        case let .pending(record):
            current = Pending(request: record.restoredRequest, body: record.body)
            pending[current.request.commandId] = current
            return try await recoverOrResend(&current)
        case let .created(record):
            current = Pending(request: record.restoredRequest, body: record.body)
            pending[current.request.commandId] = current
            try Task.checkCancellation()
            let response = try await transport.submitAction(current.request, body: current.body)
            try await applied(current, response: response)
            return response
        }
    }

    private func recoverOrResend(_ current: inout Pending) async throws -> ActionResponse {
        var held: Recorded<ActionResponse> = .unknown
        do { held = try await transport.lookupAction(current.request, body: current.body) }
        catch where ActionDeliveryDisposition(error) == .retry {}
        if case let .answered(response) = held {
            try await applied(current, response: response)
            return response
        }
        do {
            if let response = try await transport.resendAction(current.request, body: current.body) {
                try await applied(current, response: response)
                return response
            }
        } catch OperationError.sealExpired {
            guard case .absent = held, let intents, ActionOperationID(rawValue: current.request.commandId) != nil else { throw OperationError.outcomeUnknown }
            let record = try await intents.reissue(.init(request: current.request, body: current.body))
            pending.removeValue(forKey: current.request.commandId)
            current = Pending(request: record.restoredRequest, body: record.body)
            pending[current.request.commandId] = current
            scheduleRetirement()
            return try await attemptDelivery(&current)
        }
        throw OperationError.outcomeUnknown
    }

    private func discard(_ value: Pending) async {
        try? await intents?.discard(.init(request: value.request, body: value.body))
        pending.removeValue(forKey: value.request.commandId)
        scheduleRetirement()
    }

    private func publishUndelivered() {
        let cards = pending.values.filter { activeEpoch == nil || $0.request.dataEpoch == activeEpoch }.compactMap(\.request.answeredCard)
        undelivered.update(Set(cards))
    }

    public func retry(commandID: String) async throws -> ActionResponse {
        guard let value = pending[commandID] else { throw FeedError.unknownCommand }
        return try await recover(value)
    }

    public func unresolvedCommandIDs() -> [String] { pending.keys.sorted() }
    public func unresolvedRequests() -> [ActionRequest] {
        pending.keys.sorted().compactMap { pending[$0]?.request }
    }

    private func recover(_ value: Pending) async throws -> ActionResponse {
        guard let response = try await transport.recoverAction(value.request, body: value.body) else {
            throw OperationError.outcomeUnknown
        }
        try await applied(value, response: response)
        return response
    }

    private func applied(_ value: Pending, response: ActionResponse) async throws {
        try await intents?.complete(.init(request: value.request, body: value.body), response: response)
        pending.removeValue(forKey: value.request.commandId)
        if ActionOperationID(rawValue: value.request.commandId) != nil { scheduleRetirement() }
    }

    private func scheduleRetirement() {
        retirementRequested = true
        guard retirementTask == nil else { return }
        retirementTask = Task { [weak self] in await self?.runRetirement() }
    }

    private func runRetirement() async {
        while retirementRequested {
            retirementRequested = false
            await retireCompleted()
        }
        retirementTask = nil
    }

    // Tests synchronize with cleanup without making the action's UX wait for another network round trip.
    func drainRetirement() async {
        while let task = retirementTask { await task.value }
    }

    private func retireCompleted() async {
        guard let intents else { return }
        do {
            let acknowledgment = try await intents.acknowledgmentRanges()
            guard acknowledgment.generation > 0, !acknowledgment.ranges.isEmpty else { return }
            let message = ActionJournalAcknowledgment(dataEpoch: acknowledgment.dataEpoch,
                generation: acknowledgment.generation,
                ranges: acknowledgment.ranges.map { ActionOperationRange(first: $0.first, last: $0.last) })
            guard message != lastRetirement else { return }
            try await transport.acknowledgeActions(message)
            try await intents.markAcknowledged(message)
            lastRetirement = message
            retirementDeferred = false
        } catch {
            // The outcome is already committed locally; cleanup failure cannot turn it into a new send.
            retirementDeferred = true
        }
    }

    public func reconcilePending(dataEpoch: String, isCurrent: @escaping @Sendable () async -> Bool = { true }) async {
        guard !Task.isCancelled, await isCurrent() else {
            await drainRetirement()
            return
        }
        let known = journalEpoch == dataEpoch ? journalDescriptor : nil
        journalEpoch = nil; journalDescriptor = nil
        do { _ = try await journalState(dataEpoch: dataEpoch) }
        catch { if let known { journalEpoch = dataEpoch; journalDescriptor = known } }
        guard !Task.isCancelled, await isCurrent() else {
            await drainRetirement()
            return
        }
        scheduleRetirement()
        await drainRetirement()
        let candidates = pending.keys.sorted().compactMap { pending[$0] }.filter { $0.request.dataEpoch == dataEpoch }
        for value in candidates {
            guard !Task.isCancelled, await isCurrent() else {
                await drainRetirement()
                return
            }
            guard let response = try? await transport.recoverAction(value.request, body: value.body) else { continue }
            guard !Task.isCancelled, await isCurrent() else {
                await drainRetirement()
                return
            }
            try? await applied(value, response: response)
        }
        await drainRetirement()
    }
}

public extension ActionRequest {
    var deliveryCorrelationID: String { ActionOperationID.nonce(from: commandId) }
    static func deliveryNonce(_ commandID: String) -> String { ActionOperationID.nonce(from: commandID) }
}
