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
        guard !operationID.isEmpty, endpoint.path.first == "v1",
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
}
