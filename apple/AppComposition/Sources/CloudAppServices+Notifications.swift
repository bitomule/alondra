import AlondraAPIClient
import AlondraNotificationActions
import AlondraNotificationPayload
import Foundation

public extension CloudAppServices {
    /// Root derives identity/binding from the approved pairing, persists extension keys,
    /// then installs this immutable client. No presentation store is needed to act.
    func notificationClient(identity: String, binding: NotificationPayload.Binding) -> ServiceNotificationClient {
        ServiceNotificationClient(identity: identity, binding: binding, register: { token, environment in
            try await self.checkActive()
            try await self.api.registerServicePush(token: token, environment: environment, binding: binding.value)
            try await self.checkActive()
        }, submit: { action, itemID in
            await self.submitCommand(action.commandKind, itemID: itemID) { api, id in
                try await action.execute(using: api, operationID: id)
            }
        })
    }
}
