import Foundation
import AlondraContracts

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
            guard (200..<300).contains(status) else {
                #if !VIEWS_ONLY
                if ServiceRequest.isDomainReadRoute(method: endpoint.method, path: endpoint.path, query: endpoint.query,
                                                    hasBody: endpoint.body != nil)
                    || ServiceRequest.isDomainMutationRoute(method: endpoint.method, path: endpoint.path, hasBody: endpoint.body != nil)
                    || ServiceRequest.validClientContractRoute(method: endpoint.method, path: endpoint.path),
                   let error = try? DomainV2.decode(DomainV2.DomainError.self, from: body) { throw error }
                #endif
                throw APIError.from(status: status, body: body)
            }
            if endpoint.requiresJSONObjectResponse, (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] == nil {
                throw ServiceAPIError.outcomeUnknown
            }
            return try endpoint.decode(body)
        }
    }
}
