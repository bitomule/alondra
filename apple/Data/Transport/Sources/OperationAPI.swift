import Foundation

/// Infrastructure capability implemented by the durable Data coordinator. Product callers use
/// AlondraAPIClient; they do not see encrypted envelopes or manage transport reconciliation.
public protocol OperationAPI: Sendable {
    func perform(_ request: APIRequest) async throws -> APIResponse
}

public enum APIResponse: Sendable, Equatable {
    case completed(status: Int, body: Data)
    case unknown
}

public enum OperationError: Error, Sendable, Equatable {
    case operationIdentityRequired, invalidConfiguration, invalidEndpoint
    case outcomeUnknown
}

public struct APIRequest: Sendable, Equatable {
    public let operationID: String
    public let method: String
    public let path: String
    public let body: Data?

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
        if method == .post, path.count == 2, path[0] == "uploads",
           ["start", "chunk", "finish", "cancel"].contains(path[1]) { return true }
        switch (method, path) {
        case (.get, ["capabilities"]), (.get, ["changes"]), (.get, ["files", "chunk"]),
             (.post, ["actions"]), (.post, ["activity"]), (.post, ["device", "push-binding"]):
            return true
        case (.get, let rest) where rest.count == 2 && rest[0] == "views":
            return validViewKey(rest[1])
        default:
            return false
        }
    }

    private static func validViewKey(_ key: String) -> Bool {
        guard key.utf8.count <= 261 else { return false }
        if ["home", "sidebar", "tasks", "fleet", "schedules", "new_request", "issue_picker", "search", "machine"].contains(key) {
            return true
        }
        guard let colon = key.firstIndex(of: ":"),
              ["card", "document", "pull_request", "task", "request", "node", "schedule"].contains(String(key[..<colon])) else {
            return false
        }
        return validViewID(String(key[key.index(after: colon)...]))
    }

    public static func validViewID(_ id: String) -> Bool {
        guard !id.isEmpty, id.utf8.count <= 256 else { return false }
        if id.utf8.allSatisfy({ byte in
            (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57)
                || byte == 45 || byte == 95
        }) { return true }
        guard let hash = id.firstIndex(of: "#"), id[id.index(after: hash)...].firstIndex(of: "#") == nil,
              let slash = id[..<hash].firstIndex(of: "/") else { return false }
        let host = id[..<slash]
        guard !host.isEmpty, host == host.lowercased(), host.utf8.allSatisfy({ byte in
            (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57) || byte == 45 || byte == 46
        }) else { return false }
        let repo = id[id.index(after: slash)..<hash]
        guard repo == repo.lowercased() else { return false }
        let parts = repo.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 2, parts.allSatisfy({ part in
            !part.isEmpty && part != "." && part != ".." && part.utf8.allSatisfy { byte in
                (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57)
                    || byte == 45 || byte == 95 || byte == 46
            }
        }) else { return false }
        let number = id[id.index(after: hash)...]
        return !number.isEmpty && number.utf8.allSatisfy({ (48...57).contains($0) })
            && (UInt64(number).map { $0 > 0 && $0 <= 9_007_199_254_740_991 } ?? false)
    }

    public static func validClientQuery(method: HTTPMethod, path: [String], query: [(String, String)]) -> Bool {
        guard path.count == 2, path[0] == "views", method == .get else { return true }
        let names = query.map(\.0)
        guard Set(names).count == names.count,
              let protocolVersion = query.first(where: { $0.0 == "protocol" })?.1, protocolVersion == "1",
              let client = query.first(where: { $0.0 == "client" })?.1, ["ios", "mac", "tui"].contains(client),
              let lang = query.first(where: { $0.0 == "lang" })?.1, ["en", "es"].contains(lang),
              query.allSatisfy({ $0.1.utf8.count <= 256 && !$0.1.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) })
        else { return false }
        let extra = Dictionary(uniqueKeysWithValues: query.filter { !["protocol", "client", "lang", "known"].contains($0.0) })
        if let known = query.first(where: { $0.0 == "known" })?.1, known.isEmpty { return false }
        switch path[1] {
        case "home":
            return extra.isEmpty || (extra.count == 1 && extra["focus"].map(validViewID) == true)
        case "tasks":
            if extra.isEmpty { return true }
            guard extra.count == 2, let section = extra["section"], ["active", "finished", "archived"].contains(section),
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
}
