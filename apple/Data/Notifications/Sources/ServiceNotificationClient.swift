import AlondraDomain
import AlondraNotificationPayload
import Foundation

/// Immutable approved identity; replacing it invalidates completion and notification routes.
public struct ServiceNotificationClient: Sendable {
    public let identity: String
    public let binding: NotificationPayload.Binding
    public let register: @Sendable (String, String, String?, String?, UInt64) async throws -> Void
    public let submit: @Sendable (NotificationAction, NotificationPayload.Item) async -> DomainCommandResult
    public init(identity: String, binding: NotificationPayload.Binding,
                register: @escaping @Sendable (String, String, String?, String?, UInt64) async throws -> Void,
                submit: @escaping @Sendable (NotificationAction, NotificationPayload.Item) async -> DomainCommandResult) {
        self.identity = identity; self.binding = binding; self.register = register; self.submit = submit
    }
}
