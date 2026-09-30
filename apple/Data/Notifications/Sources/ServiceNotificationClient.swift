import AlondraDomain
import AlondraNotificationPayload
import AlondraViews
import Foundation

/// Immutable approved identity; replacing it invalidates completion and notification routes.
public struct ServiceNotificationClient: Sendable {
    public let identity: String
    public let binding: NotificationPayload.Binding
    public let register: @Sendable (String, String, String?, String?, UInt64) async throws -> Void
    public let submit: @Sendable (ActionRequest) async -> NotificationActionResult
    public init(identity: String, binding: NotificationPayload.Binding,
                register: @escaping @Sendable (String, String, String?, String?, UInt64) async throws -> Void,
                submit: @escaping @Sendable (ActionRequest) async -> NotificationActionResult) {
        self.identity = identity; self.binding = binding; self.register = register; self.submit = submit
    }
}
