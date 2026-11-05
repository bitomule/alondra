import Foundation

/// Control-plane APNs registration stays behind the same public API facade.
public protocol PushRegistrationAPI: Sendable {
    func registerServicePush(token: String, environment: String, binding: String) async throws
    func registerActivity(_ registration: RelayActivityRegistration) async throws
    func unregisterActivity(phoneID: String) async throws
}

public extension PushRegistrationAPI {
    func registerActivity(_ registration: RelayActivityRegistration) async throws { throw OperationError.invalidConfiguration }
    func unregisterActivity(phoneID: String) async throws { throw OperationError.invalidConfiguration }
}

public enum PushPreferencesError: Error, Sendable { case stale(currentRevision: UInt64) }

public extension AlondraAPIClient {
    /// Relay registration is an idempotent upsert. The Mac receives only the random
    /// binding through E2E; token and APNs environment are never sent to the executor.
    func registerServicePush(token: String, environment: String, binding: String, preferencesRevision: UInt64, quietStart: String? = nil, quietEnd: String? = nil, registrationID: UUID = UUID()) async throws {
        guard let push = operations as? any PushRegistrationAPI else { throw OperationError.invalidConfiguration }
        guard (1...9_007_199_254_740_991).contains(preferencesRevision), (quietStart == nil) == (quietEnd == nil) else { throw OperationError.invalidConfiguration }
        try await push.registerServicePush(token: token, environment: environment, binding: binding)
        let body = try JSONSerialization.data(withJSONObject: [
            "push_binding": binding, "preferences_revision": preferencesRevision, "quiet_start": quietStart as Any? ?? NSNull(), "quiet_end": quietEnd as Any? ?? NSNull()
        ], options: [.sortedKeys])
        let endpoint = Endpoint<Data>.data(.post, path: ["device", "push-binding"], body: body, contentType: "application/json")
        let request = try APIRequest(endpoint: endpoint, operationID: registrationID.uuidString)
        try Task.checkCancellation()
        switch try await operations.perform(request) {
        case .unknown: throw OperationError.outcomeUnknown
        case .completed(let status, let response):
            if status == 409,
               let object = (try? JSONSerialization.jsonObject(with: response)) as? [String: Any],
               object["kind"] as? String == "stale_push_preferences",
               let current = object["current_revision"] as? NSNumber,
               let revision = UInt64(current.stringValue), revision <= 9_007_199_254_740_991 {
                throw PushPreferencesError.stale(currentRevision: revision)
            }
            guard (200..<300).contains(status) else { throw APIError.from(status: status, body: response) }
        }
    }

    /// The Live Activity's tokens go to the relay alone, like the push token: the Mac never
    /// learns them.
    func registerActivity(_ registration: RelayActivityRegistration) async throws {
        guard let push = operations as? any PushRegistrationAPI else { throw OperationError.invalidConfiguration }
        try await push.registerActivity(registration)
    }

    func unregisterActivity(phoneID: String) async throws {
        guard let push = operations as? any PushRegistrationAPI else { throw OperationError.invalidConfiguration }
        try await push.unregisterActivity(phoneID: phoneID)
    }

    /// Sign-out tells the Mac to stop pushing to this device before its session is gone.
    /// It names the binding so a Mac re-paired since keeps the newer one.
    func removeServicePushBinding(_ binding: String) async throws {
        let body = try JSONSerialization.data(withJSONObject: ["push_binding": binding])
        let endpoint = Endpoint<Data>.data(.post, path: ["device", "push-binding", "remove"], body: body,
                                           contentType: "application/json")
        let request = try APIRequest(endpoint: endpoint, operationID: UUID().uuidString)
        switch try await operations.perform(request) {
        case .unknown: throw OperationError.outcomeUnknown
        case .completed(let status, let response):
            guard (200..<300).contains(status) else { throw APIError.from(status: status, body: response) }
        }
    }
}
