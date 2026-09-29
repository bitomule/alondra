import Foundation

/// Infrastructure capability implemented by the durable Data coordinator. Product callers use
/// AlondraAPIClient; they do not see encrypted envelopes or manage transport reconciliation.
public protocol ServiceOperationTransport: Sendable {
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse
}

public enum ServiceResponse: Sendable, Equatable {
    case completed(status: Int, body: Data)
    case unknown
}

public enum ServiceAPIError: Error, Sendable, Equatable {
    case operationIdentityRequired, invalidConfiguration, invalidEndpoint
    case outcomeUnknown
}

public struct ServiceRequest: Sendable, Equatable {
    public let operationID: String
    public let method: String
    public let path: String
    public let body: Data?

    init<Response>(endpoint: Endpoint<Response>, operationID: String) throws {
        let domainV2 = endpoint.path == ["capabilities"] && endpoint.method == .get
            || endpoint.path == ["changes"] && endpoint.method == .get
        let domainDetail = endpoint.method == .get && endpoint.path.count == 3
            && ((endpoint.path[0] == "pull-requests" && endpoint.path[2] == "diff")
                || (endpoint.path[0] == "document-revisions" && endpoint.path[2] == "content")
                || (endpoint.path[0] == "tasks" && endpoint.path[2] == "actions"))
        let domainDelivery = endpoint.path == ["delivery-attempts"] && endpoint.method == .get
            || (endpoint.method == .post && endpoint.path.count == 3
                && endpoint.path.starts(with: ["delivery-attempts"])
                && (Int64(endpoint.path[1]).map { $0 > 0 } ?? false) && endpoint.path[2] == "resolve")
        let domainMutation = endpoint.query.isEmpty && Self.validResourceMutationRoute(method: endpoint.method, path: endpoint.path)
        let domainSchedule = endpoint.query.isEmpty && Self.validDomainScheduleRoute(method: endpoint.method,
            path: endpoint.path, hasBody: endpoint.body != nil)
        let clientContract = Self.validClientContractRoute(method: endpoint.method, path: endpoint.path)
        // A logical executor path, never a QR-provided URL or bearer destination.
        let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        let segments = endpoint.path.map { $0.addingPercentEncoding(withAllowedCharacters: unreserved)! }
        let query = endpoint.query.map {
            $0.0.addingPercentEncoding(withAllowedCharacters: unreserved)! + "=" +
            $0.1.addingPercentEncoding(withAllowedCharacters: unreserved)!
        }.joined(separator: "&")
        let path = "/" + segments.joined(separator: "/") + (query.isEmpty ? "" : "?" + query)
        let domainRead = endpoint.method == .get && endpoint.body == nil && DomainReadPath.isValid(path)
        guard !operationID.isEmpty, (domainV2 || domainRead || domainDetail || domainDelivery || domainMutation || domainSchedule || clientContract),
              endpoint.path.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
            throw ServiceAPIError.invalidEndpoint
        }
        self.operationID = operationID
        self.method = endpoint.method.rawValue
        self.path = path
        self.body = endpoint.body
    }

    /// The v2 daemon's routes beyond the domain (docs/domain-v2-apple-client-contracts.md).
    public static func validClientContractRoute(method: HTTPMethod, path: [String]) -> Bool {
        if method == .post, path.count == 3, path[0] == "uploads",
           !path[1].isEmpty, ["start", "chunk", "finish", "cancel"].contains(path[2]) { return true }
        guard path.starts(with: ["v2"]), path.count >= 2 else { return false }
        switch (method, Array(path.dropFirst())) {
        case (.get, ["runtime"]), (.get, ["agents", "search"]), (.get, ["projects"]), (.get, ["options"]),
             (.get, ["issues"]), (.post, ["requests"]), (.post, ["device", "push-binding"]), (.post, ["orchestrator", "messages"]):
            return true
        case (.get, let rest) where rest.count == 3 && rest[0] == "agents" && rest[2] == "activity":
            return true
        case (.post, let rest) where rest.count == 3 && rest[0] == "agents" && ["messages", "close"].contains(rest[2]):
            return true
        case (.get, let rest) where rest.count == 3 && rest[0] == "tasks" && rest[2] == "history":
            return true
        case (.get, ["files", "chunk"]):
            return true
        case (.post, let rest) where rest.count == 2 && rest[0] == "orchestrator":
            return OrchestratorDecision(rawValue: rest[1]) != nil
        default:
            return false
        }
    }

    public static func validResourceMutationPath(method: HTTPMethod, path: String) -> Bool {
        let route = path.split(separator: "?", maxSplits: 1).first.map(String.init) ?? path
        let segments = route.split(separator: "/", omittingEmptySubsequences: false).dropFirst().map(String.init)
        guard !path.contains("?") else { return false }
        return validResourceMutationRoute(method: method, path: segments)
    }

    public static func isDomainMutationRoute(method: HTTPMethod, path: [String], hasBody: Bool) -> Bool {
        validResourceMutationRoute(method: method, path: path)
            || validDomainScheduleRoute(method: method, path: path, hasBody: hasBody)
    }

    /// Exact resource mutations admitted by the daemon's resource router.
    public static func validResourceMutationRoute(method: HTTPMethod, path: [String]) -> Bool {
        switch (method, path) {
        case (.post, ["requests"]), (.post, ["questions"]), (.post, ["action-requests"]), (.post, ["messages"]):
            return true
        case (.post, ["requests", _, "close"]), (.post, ["requests", _, "tasks"]):
            return validResourceID(path[1])
        case (.post, ["tasks", _, "start"]), (.post, ["tasks", _, "documents"]),
             (.post, ["tasks", _, "document-links"]), (.post, ["tasks", _, "pull-requests"]),
             (.post, ["tasks", _, "results"]), (.post, ["tasks", _, "stack-readiness"]),
             (.post, ["tasks", _, "completion"]), (.post, ["tasks", _, "cancellation"]),
             (.post, ["tasks", _, "reopening"]), (.post, ["tasks", _, "archival"]),
             (.post, ["tasks", _, "questions"]), (.post, ["tasks", _, "action-requests"]),
             (.post, ["tasks", _, "messages"]), (.post, ["merge-authorizations", _, "claim"]),
             (.put, ["tasks", _, "assignee"]), (.put, ["tasks", _, "dependencies"]),
             (.put, ["tasks", _, "documents", _]), (.put, ["tasks", _, "documents", _, "review"]),
             (.patch, ["tasks", _]), (.post, ["interactions", _, "answers"]),
             (.post, ["interactions", _, "withdrawal"]), (.post, ["interactions", _, "feedback"]),
             (.post, ["interactions", _, "acceptance"]), (.post, ["interactions", _, "merge-authorizations"]),
             (.put, ["interactions", _, "subject"]), (.put, ["interactions", _, "read-state"]):
            let ids = [path[1]] + (path.count >= 4 && path[2] == "documents" ? [path[3]] : [])
            return ids.allSatisfy(validResourceID)
        default:
            return false
        }
    }

    private static func validResourceID(_ value: String) -> Bool {
        (1...256).contains(value.utf8.count) && value.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte) || byte == 45 || byte == 95
        }
    }

    private static func validDomainScheduleRoute(method: HTTPMethod, path: [String], hasBody: Bool) -> Bool {
        guard path.starts(with: ["schedules"]) else { return false }
        if path.count == 1 {
            return (method == .get && !hasBody) || (method == .post && hasBody)
        }
        guard path.count == 2 || path.count == 3,
              (1...256).contains(path[1].utf8.count),
              path[1].utf8.allSatisfy({ byte in
                  (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122)
                    || (byte >= 48 && byte <= 57) || byte == 45 || byte == 95
              }) else { return false }
        if path.count == 3 {
            return path[2] == "runs" && method == .post && !hasBody
        }
        return (method == .get && !hasBody) || (method == .put && hasBody)
            || (method == .delete && !hasBody)
    }
}
