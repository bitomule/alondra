import Foundation

/// Infrastructure capability implemented by the durable Data coordinator. Product callers use
/// AlondraAPIClient; they do not see encrypted envelopes or manage transport reconciliation.
public protocol ServiceOperationTransport: Sendable {
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws
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
        let domainV2 = endpoint.path == ["v2", "domain"] && endpoint.method == .get
            || endpoint.path == ["v2", "domain", "capabilities"] && endpoint.method == .get
            || endpoint.path == ["v2", "domain", "changes"] && endpoint.method == .get
            || endpoint.path == ["v2", "domain", "commands"] && endpoint.method == .post
        let domainDetail = endpoint.method == .get && endpoint.path.count == 5
            && endpoint.path.starts(with: ["v2", "domain"])
            && ((endpoint.path[2] == "pull-requests" && endpoint.path[4] == "diff")
                || (endpoint.path[2] == "document-revisions" && endpoint.path[4] == "content")
                || (endpoint.path[2] == "tasks" && endpoint.path[4] == "actions"))
        let domainDelivery = endpoint.path == ["v2", "domain", "delivery-attempts"] && endpoint.method == .get
            || (endpoint.method == .post && endpoint.path.count == 5
                && endpoint.path.starts(with: ["v2", "domain", "delivery-attempts"])
                && (Int64(endpoint.path[3]).map { $0 > 0 } ?? false) && endpoint.path[4] == "resolve")
        let domainSchedule = endpoint.query.isEmpty && Self.validDomainScheduleRoute(method: endpoint.method,
            path: endpoint.path, hasBody: endpoint.body != nil)
        guard !operationID.isEmpty, (endpoint.path.first == "v1" || domainV2 || domainDetail || domainDelivery || domainSchedule),
              endpoint.path.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
            throw ServiceAPIError.invalidEndpoint
        }
        // A logical executor path, never a QR-provided URL or bearer destination.
        let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        let segments = endpoint.path.map { $0.addingPercentEncoding(withAllowedCharacters: unreserved)! }
        let query = endpoint.query.map {
            $0.0.addingPercentEncoding(withAllowedCharacters: unreserved)! + "=" +
            $0.1.addingPercentEncoding(withAllowedCharacters: unreserved)!
        }.joined(separator: "&")
        self.operationID = operationID
        self.method = endpoint.method.rawValue
        self.path = "/" + segments.joined(separator: "/") + (query.isEmpty ? "" : "?" + query)
        self.body = endpoint.body
    }

    private static func validDomainScheduleRoute(method: HTTPMethod, path: [String], hasBody: Bool) -> Bool {
        guard path.starts(with: ["v2", "domain", "schedules"]) else { return false }
        if path.count == 3 {
            return (method == .get && !hasBody) || (method == .post && hasBody)
        }
        guard path.count == 4 || path.count == 5,
              (1...256).contains(path[3].utf8.count),
              path[3].utf8.allSatisfy({ byte in
                  (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122)
                    || (byte >= 48 && byte <= 57) || byte == 45 || byte == 95
              }) else { return false }
        if path.count == 5 {
            return path[4] == "run" && method == .post && !hasBody
        }
        return (method == .get && !hasBody) || (method == .put && hasBody)
            || (method == .delete && !hasBody)
    }
}
