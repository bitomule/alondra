import Foundation

/// Infrastructure capability implemented by the durable Data coordinator. Product callers use
/// AlondraAPIClient; they do not see encrypted envelopes or manage transport reconciliation.
public protocol OperationAPI: Sendable {
    func perform(_ request: APIRequest) async throws -> APIResponse
    func recover(_ request: APIRequest) async throws -> APIResponse
    /// Sends again the sealed bytes of an action this device already sent: the relay and the Mac
    /// recognise them and never run it twice. Throws `OperationError.sealExpired` once they are too
    /// old to be accepted, and is `.unknown` when nothing was sealed for it.
    func resend(_ request: APIRequest) async throws -> APIResponse
    /// What the Mac holds for an action this device sent, telling apart "no result" from no answer.
    func lookup(_ request: APIRequest) async throws -> OperationLookup
    func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws
}

public extension OperationAPI {
    func recover(_ request: APIRequest) async throws -> APIResponse { .unknown }
    func resend(_ request: APIRequest) async throws -> APIResponse { .unknown }
    func lookup(_ request: APIRequest) async throws -> OperationLookup {
        switch try await recover(request) {
        case let .completed(status, body): .completed(status: status, body: body)
        case .unknown: .unknown
        }
    }
    func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {}
}

public enum APIResponse: Sendable, Equatable {
    case completed(status: Int, body: Data)
    case unknown
}

/// What the Mac said about an action this device sent.
public enum OperationLookup: Sendable, Equatable {
    case completed(status: Int, body: Data)
    /// The Mac answered that it holds no finished result for it.
    case absent
    case unknown

    var response: APIResponse {
        if case let .completed(status, body) = self { return .completed(status: status, body: body) }
        return .unknown
    }
}

public enum OperationError: LocalizedError, Sendable, Equatable {
    case operationIdentityRequired, invalidConfiguration, invalidEndpoint
    case outcomeUnknown, sealExpired

    public var errorDescription: String? {
        switch self {
        case .outcomeUnknown: String(localized: "The Mac didn’t confirm it. Try again.")
        case .operationIdentityRequired, .invalidConfiguration, .invalidEndpoint, .sealExpired:
            String(localized: "Alondra couldn’t prepare this request.")
        }
    }
}

public struct APIRequest: Sendable, Equatable {
    public let operationID: String
    public let method: String
    public let path: String
    public let body: Data?

    var isActionJournalControl: Bool {
        method == "GET" && path.hasPrefix("/operation-journal?") && body == nil
            || method == "POST" && path == "/operation-journal/ack" && body != nil
    }

    init<Response>(endpoint: Endpoint<Response>, operationID: String) throws {
        let resourceRead = endpoint.path == ["capabilities"] && endpoint.method == .get
            || endpoint.path == ["changes"] && endpoint.method == .get
        let contentDetail = endpoint.method == .get && endpoint.path.count == 3
            && ((endpoint.path[0] == "pull-requests" && endpoint.path[2] == "diff")
                || (endpoint.path[0] == "document-revisions" && endpoint.path[2] == "content"))
            && Self.validViewID(endpoint.path[1])
        let clientContract = Self.validClientContractRoute(method: endpoint.method, path: endpoint.path)
            && Self.validClientQuery(method: endpoint.method, path: endpoint.path, query: endpoint.query)
        // A logical executor path, never a QR-provided URL or bearer destination.
        let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        let segments = endpoint.path.map { $0.addingPercentEncoding(withAllowedCharacters: unreserved)! }
        let query = endpoint.query.map {
            $0.0.addingPercentEncoding(withAllowedCharacters: unreserved)! + "=" +
            $0.1.addingPercentEncoding(withAllowedCharacters: unreserved)!
        }.joined(separator: "&")
        let path = "/" + segments.joined(separator: "/") + (query.isEmpty ? "" : "?" + query)
        guard !operationID.isEmpty, (resourceRead || contentDetail || clientContract),
              endpoint.path.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
            throw OperationError.invalidEndpoint
        }
        self.operationID = operationID
        self.method = endpoint.method.rawValue
        self.path = path
        self.body = endpoint.body
    }

    /// The daemon routes available to the Views client.
    public static func validClientContractRoute(method: HTTPMethod, path: [String]) -> Bool {
        if method == .get, path == ["operation-journal"] { return true }
        if method == .post, path == ["operation-journal", "ack"] { return true }
        if method == .post, path.count == 2, path[0] == "uploads",
           ["start", "chunk", "finish", "cancel"].contains(path[1]) { return true }
        if method == .get, path.count == 2, path[0] == "operation-results" {
            return validOperationID(path[1])
        }
        if method == .post, path.count == 3, path[0] == "mcp-sign-ins",
           path[1].utf8.count <= 256, path[1].allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }) {
            if path[1].hasPrefix("mcp-sign-in-"), ["connect", "return", "reconnect", "without"].contains(path[2]) { return true }
            if path[1].hasPrefix("mcp-server-"), ["connect", "return", "reconnect"].contains(path[2]) { return true }
        }
        switch (method, path) {
        case (.get, ["capabilities"]), (.get, ["changes"]), (.get, ["files", "chunk"]), (.get, ["routes"]),
             (.post, ["actions"]), (.post, ["activity"]), (.post, ["device", "push-binding"]),
             (.post, ["device", "push-binding", "remove"]),
             (.get, ["settings"]), (.put, ["settings"]), (.get, ["settings", "folders"]):
            return true
        case (.get, let rest) where rest.count == 2 && rest[0] == "views":
            return validViewKey(rest[1])
        default:
            return false
        }
    }

    private static func validViewKey(_ key: String) -> Bool {
        guard key.utf8.count <= 261 else { return false }
        if ["home", "sidebar", "tasks", "schedules", "new_request", "issue_picker", "search", "machine", "mcp_servers", "accounts"].contains(key) {
            return true
        }
        guard let colon = key.firstIndex(of: ":"),
              ["card", "document", "pull_request", "task", "request", "node", "schedule"].contains(String(key[..<colon])) else {
            return false
        }
        return validViewID(String(key[key.index(after: colon)...]))
    }

    public static func validViewID(_ id: String) -> Bool {
        let bytes = Array(id.utf8)
        guard !bytes.isEmpty, bytes.count <= 256 else { return false }
        func plain(_ byte: UInt8) -> Bool {
            (65...90).contains(byte) || (97...122).contains(byte) || (48...57).contains(byte)
                || byte == 45 || byte == 95 || byte == 46
        }
        if bytes.allSatisfy(plain), id != ".", id != ".." { return true }
        return hostedPullRequestID(bytes)
    }

    private static func hostedPullRequestID(_ bytes: [UInt8]) -> Bool {
        guard let hash = bytes.firstIndex(of: 35) else { return false }
        let repo = bytes[..<hash], number = bytes[(hash + 1)...]
        guard let slash = repo.firstIndex(of: 47) else { return false }
        let host = repo[..<slash], path = repo[(slash + 1)...]
        guard !host.isEmpty,
              host.allSatisfy({ (97...122).contains($0) || (48...57).contains($0) || $0 == 46 || $0 == 45 }),
              !path.contains(where: { (65...90).contains($0) }) else { return false }
        let parts = path.split(separator: 47, omittingEmptySubsequences: false)
        guard parts.count == 2, parts.allSatisfy({ part in
            !part.isEmpty && !part.elementsEqual([46]) && !part.elementsEqual([46, 46]) && part.allSatisfy { byte in
                (65...90).contains(byte) || (97...122).contains(byte) || (48...57).contains(byte)
                    || byte == 45 || byte == 95 || byte == 46
            }
        }) else { return false }
        guard !number.isEmpty, number.allSatisfy({ (48...57).contains($0) }),
              let value = UInt64(String(decoding: number, as: UTF8.self)) else { return false }
        return value > 0 && value <= 9_007_199_254_740_991
    }

    public static func validClientQuery(method: HTTPMethod, path: [String], query: [(String, String)]) -> Bool {
        if path.first == "operation-journal" {
            if method == .post { return path == ["operation-journal", "ack"] && query.isEmpty }
            return method == .get && path == ["operation-journal"] && query.count == 1
                && query[0].0 == "data_epoch" && RelayHTTPChallenge.validText(query[0].1)
        }
        if path.first == "operation-results" {
            guard method == .get, path.count == 2, validOperationID(path[1]),
                  (2...3).contains(query.count), Set(query.map(\.0)).count == query.count,
                  Set(query.map(\.0)).isSubset(of: ["digest", "data_epoch", "offset"]),
                  let digest = query.first(where: { $0.0 == "digest" })?.1,
                  digest.utf8.count == 64, digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
                  let epoch = query.first(where: { $0.0 == "data_epoch" })?.1,
                  RelayHTTPChallenge.validText(epoch) else { return false }
            if let offset = query.first(where: { $0.0 == "offset" })?.1 {
                guard !offset.isEmpty, offset.utf8.allSatisfy({ (48...57).contains($0) }),
                      let value = Int(offset), value <= DirectHTTP.maxResponse, value % (256 * 1024) == 0 else { return false }
            }
            return true
        }
        if path.first == "settings" { return validSettingsQuery(path: path, query: query) }
        guard path.count == 2, path[0] == "views", method == .get else { return true }
        let names = query.map(\.0)
        guard Set(names).count == names.count,
              let protocolVersion = query.first(where: { $0.0 == "protocol" })?.1, protocolVersion == "1",
              let client = query.first(where: { $0.0 == "client" })?.1, ["ios", "mac", "tui"].contains(client),
              let lang = query.first(where: { $0.0 == "lang" })?.1, ["en", "es"].contains(lang),
              query.allSatisfy({ $0.1.utf8.count <= 256 && !$0.1.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) })
        else { return false }
        if let capabilities = query.first(where: { $0.0 == "capabilities" })?.1,
           !validViewCapabilities(capabilities) { return false }
        let extra = Dictionary(uniqueKeysWithValues: query.filter { !["protocol", "client", "lang", "known", "capabilities"].contains($0.0) })
        if let known = query.first(where: { $0.0 == "known" })?.1, known.isEmpty { return false }
        switch path[1] {
        case "home":
            return extra.isEmpty || (extra.count == 1 && extra["focus"].map(validViewID) == true)
        case "tasks":
            if extra.isEmpty { return true }
            guard extra.count == 2, let section = extra["section"], ["active", "finished"].contains(section),
                  let after = extra["after"], UInt(after) != nil else { return false }
            return true
        case "new_request", "schedule:new":
            return extra.isEmpty || (extra.count == 1 && extra["project"] != nil)
        case "issue_picker":
            return extra.isEmpty || extra == ["refresh": "1"] || (extra.count == 1 && extra["q"] != nil)
        case "search":
            return extra.isEmpty || (extra.count == 1 && extra["q"] != nil)
        default:
            return extra.isEmpty
        }
    }

    static func validViewCapabilities(_ value: String) -> Bool {
        guard value.utf8.count <= 256 else { return false }
        if value.isEmpty { return true }
        return value.split(separator: ",", omittingEmptySubsequences: false).allSatisfy {
            !$0.isEmpty && $0.utf8.count <= 64 && $0.utf8.allSatisfy { (97...122).contains($0) || $0 == 95 }
        }
    }

    static func validOperationID(_ id: String) -> Bool {
        !id.isEmpty && id.utf8.count <= 128 && id.utf8.allSatisfy {
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
        }
    }

    private static func validSettingsQuery(path: [String], query: [(String, String)]) -> Bool {
        if path == ["settings", "folders"], query.count == 1, let (name, value) = query.first {
            return name == "path" && !value.isEmpty && value.utf8.count <= 4096
                && !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
        }
        return query.isEmpty
    }
}
