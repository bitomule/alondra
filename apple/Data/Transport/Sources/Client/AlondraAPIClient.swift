import Foundation
import AlondraContract

/// Product API over the durable service operation transport. HTTP authentication,
/// envelopes and journaling belong to the capabilities composed beneath this value.
public struct AlondraAPIClient: Sendable {
    public let base: URL
    public let device: String
    let operations: any OperationAPI

    public init(serviceURL: URL, deviceID: String, operations: any OperationAPI) {
        self.base = serviceURL
        self.device = deviceID
        self.operations = operations
    }

    public func send<Response>(_ endpoint: Endpoint<Response>, operationID: String? = nil) async throws -> Response {
        guard endpoint.method == .get || operationID?.isEmpty == false else {
            throw OperationError.operationIdentityRequired
        }
        let request = try APIRequest(endpoint: endpoint, operationID: operationID ?? UUID().uuidString)
        try Task.checkCancellation()
        switch try await operations.perform(request) {
        case .unknown: throw OperationError.outcomeUnknown
        case let .completed(status, body):
            guard (200..<300).contains(status) else {
                throw APIError.from(status: status, body: body)
            }
            if endpoint.requiresJSONObjectResponse, (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] == nil {
                throw OperationError.outcomeUnknown
            }
            return try endpoint.decode(body)
        }
    }
}
