import AlondraNotificationPayload

/// Identifies the registration attempt whose completion may update app state.
public struct PushRegistrationScope: Equatable, Sendable {
    public let clientIdentity: String
    public let binding: NotificationPayload.Binding
    public let token: String

    public init(clientIdentity: String, binding: NotificationPayload.Binding, token: String) {
        self.clientIdentity = clientIdentity
        self.binding = binding
        self.token = token
    }

    public func acceptsCompletion(current: PushRegistrationScope?, cancelled: Bool) -> Bool {
        !cancelled && current == self
    }
}
