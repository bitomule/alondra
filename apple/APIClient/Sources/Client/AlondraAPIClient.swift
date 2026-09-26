import Foundation

/// Product API over the durable service operation transport. HTTP authentication,
/// envelopes and journaling belong to the capabilities composed beneath this value.
public struct AlondraAPIClient: Sendable {
    public let base: URL
    public let device: String
    let operations: any ServiceOperationTransport

    public init(serviceURL: URL, deviceID: String, operations: any ServiceOperationTransport) {
        self.base = serviceURL
        self.device = deviceID
        self.operations = operations
    }

    public func send<Response>(_ endpoint: Endpoint<Response>, operationID: String? = nil) async throws -> Response {
        guard endpoint.method == .get || operationID?.isEmpty == false else {
            throw ServiceAPIError.operationIdentityRequired
        }
        let request = try ServiceRequest(endpoint: endpoint, operationID: operationID ?? UUID().uuidString)
        try Task.checkCancellation()
        switch try await operations.perform(request) {
        case .unknown: throw ServiceAPIError.outcomeUnknown
        case let .completed(status, body):
            guard (200..<300).contains(status) else { throw APIError.from(status: status, body: body) }
            return try endpoint.decode(body)
        }
    }
}
