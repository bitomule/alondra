import AlondraContract
import AlondraTransport
import Foundation

/// Immutable approved identity; replacing it invalidates completion and notification routes.
public struct NotificationClient: Sendable {
    public let identity: String
    public let binding: NotificationPayload.Binding
    public let register: @Sendable (String, String, String?, String?, UInt64) async throws -> Void
    public let submit: @Sendable (ActionRequest) async -> NotificationActionResult
    public let registerActivity: @Sendable (RelayActivityRegistration) async throws -> Void
    public let unregisterActivity: @Sendable (String) async throws -> Void
    public let giveActivityKey: @Sendable (String) async throws -> Void
    public init(identity: String, binding: NotificationPayload.Binding,
                register: @escaping @Sendable (String, String, String?, String?, UInt64) async throws -> Void,
                submit: @escaping @Sendable (ActionRequest) async -> NotificationActionResult,
                registerActivity: @escaping @Sendable (RelayActivityRegistration) async throws -> Void = { _ in },
                unregisterActivity: @escaping @Sendable (String) async throws -> Void = { _ in },
                giveActivityKey: @escaping @Sendable (String) async throws -> Void = { _ in }) {
        self.identity = identity; self.binding = binding; self.register = register; self.submit = submit
        self.registerActivity = registerActivity; self.unregisterActivity = unregisterActivity
        self.giveActivityKey = giveActivityKey
    }
}
