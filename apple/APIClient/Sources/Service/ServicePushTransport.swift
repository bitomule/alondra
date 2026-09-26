import Foundation

/// Control-plane APNs registration stays behind the same public API facade.
public protocol ServicePushTransport: Sendable {
    func registerServicePush(token: String, environment: String, binding: String) async throws
}

public extension AlondraAPIClient {
    /// Relay registration is an idempotent upsert. The Mac receives only the random
    /// binding through E2E; token and APNs environment are never sent to the executor.
    func registerServicePush(token: String, environment: String, binding: String) async throws {
        guard let push = operations as? any ServicePushTransport else { throw ServiceAPIError.invalidConfiguration }
        try await push.registerServicePush(token: token, environment: environment, binding: binding)
        let body = try JSONSerialization.data(withJSONObject: ["push_binding": binding])
        _ = try await send(Endpoint<Data>.data(.post, path: ["v1", "service", "push-binding"],
            body: body, contentType: "application/json"), operationID: "push-binding-" + binding)
    }
}
