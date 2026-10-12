import AlondraTransport
import AlondraContract
import CryptoKit
import Foundation

/// Transport boundary used by the feed and action actors. The API client implementation
/// preserves the view/action bytes and operation identity through loopback or relay.
public protocol ViewTransport: Sendable {
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState?
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws
    func recoverAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse?
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse
    func fetchContent(_ content: ContentRef) async throws -> String
    func fetchAttachment(_ attachment: AlondraContract.Attachment) async throws -> Data
}

public extension ViewTransport {
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState? { nil }
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws { throw OperationError.invalidEndpoint }
    func recoverAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? { nil }
    func fetchContent(_ content: ContentRef) async throws -> String { throw FeedError.unsupportedContent }
    func fetchAttachment(_ attachment: AlondraContract.Attachment) async throws -> Data { throw FeedError.unsupportedContent }
}

extension AlondraAPIClient: ViewTransport {
    public func fetchView(key: String, client: ViewClient, lang: String, known: String? = nil) async throws -> ViewFetch {
        guard let route = ViewRoute.parse(key) else { throw FeedError.invalidKey }
        let query: [(String, String)] = [("protocol", String(ContractCodec.protocolVersion)), ("client", client.rawValue), ("lang", lang)]
            + route.query
            + (known.map { [("known", $0)] } ?? [])
        return try await send(Endpoint<ViewFetch>(.get, path: ["views", route.path], query: query) {
            try ContractCodec.decode(ViewFetch.self, from: $0)
        })
    }

    public func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        precondition(wait <= 50)
        let viewList = views.map(\.feedValue).sorted().joined(separator: ",")
        let query: [(String, String)] = [("protocol", String(ContractCodec.protocolVersion)), ("client", client.rawValue), ("lang", lang), ("wait", String(wait))]
            + (cursor.map { [("cursor", $0)] } ?? [])
            + (viewList.isEmpty ? [] : [("views", viewList)])
        return try await send(Endpoint<ViewFeed>(.get, path: ["changes"], query: query) {
            try ContractCodec.decode(ViewFeed.self, from: $0)
        })
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
            guard path.count == 3, path[0] == "pull-requests", path[2] == "diff",
                  let rawID = Self.decodedPullRequestID(from: content.fetch), let expectedHead = content.digest else {
                throw FeedError.unsupportedContent
            }
            let diff = try await send(Endpoint<DiffResponse>(.get, path: ["pull-requests", rawID, "diff"]) {
                try JSONDecoder().decode(DiffResponse.self, from: $0)
            })
            guard diff.prID == rawID, diff.head == expectedHead else { throw FeedError.invalidContent }
            return diff.diff
        case .file:
            guard path.first == "files", let digest = content.digest else { throw FeedError.unsupportedContent }
            let data = try await readFileChunks(reference: content.fetch, digest: digest)
            guard let text = String(data: data, encoding: .utf8) else { throw FeedError.invalidContent }
            return text
        }
    }

    public func fetchAttachment(_ attachment: AlondraContract.Attachment) async throws -> Data {
        guard let digest = attachment.digest, attachment.fetch.hasPrefix("/"), attachment.fetch.hasPrefix("/files/") else {
            throw FeedError.unsupportedContent
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
              ViewRoute.validID(decoded) else { return nil }
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
            guard values.count == 2, let section = values["section"], ["active", "finished", "archived"].contains(section),
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

    static func validID(_ id: String) -> Bool {
        guard !id.isEmpty, id.utf8.count <= 256 else { return false }
        if id.utf8.allSatisfy({ byte in
            (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57)
                || byte == 45 || byte == 95
        }) { return true }
        guard id.hasPrefix("github.com/"), let hash = id.firstIndex(of: "#") else { return false }
        let repo = id[id.index(id.startIndex, offsetBy: "github.com/".count)..<hash]
        let parts = repo.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 2, parts.allSatisfy({ part in
            !part.isEmpty && part != "." && part != ".." && part.utf8.allSatisfy { byte in
                (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57)
                    || byte == 45 || byte == 95 || byte == 46
            }
        }) else { return false }
        let number = id[id.index(after: hash)...]
        return !number.isEmpty && number.utf8.allSatisfy({ (48...57).contains($0) })
            && (UInt64(number).map { $0 > 0 } ?? false)
    }
}

/// Canonical intent is stored before its first dispatch. An unresolved intent is recovered by GET,
/// including when a new screen supplies another UUID for the same action after relaunch.
public actor ViewActionSender {
    private struct Pending: Equatable, Sendable { let request: ActionRequest; let body: Data }
    private let transport: any ViewTransport
    private let intents: ViewActionIntentStore?
    private var pending: [String: Pending]
    private var retirementDeferred = false
    private var retirementRequested = false
    private var retirementTask: Task<Void, Never>?
    private var lastRetirement: ActionJournalAcknowledgment?
    private var journalEpoch: String?
    private var journalDescriptor: ActionJournalState?

    public init(transport: any ViewTransport, intents: ViewActionIntentStore? = nil) {
        self.transport = transport; self.intents = intents
        pending = Dictionary(uniqueKeysWithValues: (intents?.restored ?? []).map {
            ($0.request.commandId, Pending(request: $0.restoredRequest, body: $0.body))
        })
    }

    private func journalState(dataEpoch: String) async throws -> ActionJournalState? {
        if journalEpoch == dataEpoch { return journalDescriptor }
        let descriptor = try await transport.actionJournal(dataEpoch: dataEpoch)
        try Task.checkCancellation()
        journalEpoch = dataEpoch; journalDescriptor = descriptor
        return descriptor
    }

    public func prepare(_ request: ActionRequest) async throws -> ActionRequest {
        try Task.checkCancellation()
        guard let intents else { return request }
        if ActionOperationID(rawValue: request.commandId) != nil { return request }
        let descriptor = try await journalState(dataEpoch: request.dataEpoch)
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
        journalEpoch = nil; journalDescriptor = nil
        _ = try? await journalState(dataEpoch: dataEpoch)
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
