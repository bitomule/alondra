import AlondraCommands
import AlondraNotificationPayload
import Foundation

/// Immutable approved identity; replacing it invalidates completion and notification routes.
public struct ServiceNotificationClient: Sendable {
    public let identity: String
    public let binding: NotificationPayload.Binding
    public let register: @Sendable (String, String, String?, String?) async throws -> Void
    public let submit: @Sendable (NotificationAction, String) async -> CommandResult
    public init(identity: String, binding: NotificationPayload.Binding,
                register: @escaping @Sendable (String, String, String?, String?) async throws -> Void,
                submit: @escaping @Sendable (NotificationAction, String) async -> CommandResult) {
        self.identity = identity; self.binding = binding; self.register = register; self.submit = submit
    }
}
