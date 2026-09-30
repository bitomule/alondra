import AlondraAPIClient
import AlondraNotificationActions
import AlondraNotificationPayload
import AlondraViewsData
import Foundation

public extension CloudAppServices {
    /// Root derives identity/binding from the approved pairing, persists extension keys,
    /// then installs this immutable client. No presentation store is needed to act.
    func notificationClient(identity: String, binding: NotificationPayload.Binding) -> ServiceNotificationClient {
        ServiceNotificationClient(identity: identity, binding: binding, register: { token, environment, quietStart, quietEnd, revision in
            try await self.checkActive()
            try await self.api.registerServicePush(token: token, environment: environment, binding: binding.value, preferencesRevision: revision, quietStart: quietStart, quietEnd: quietEnd)
            try await self.checkActive()
        }, submit: { request in
            do {
                try await self.checkActive()
                let response = try await self.viewActionSender.send(request)
                try await self.checkActive()
                if response.outcome == .noLongerAvailable { return .noLongerPending }
                return .sent(response)
            } catch {
                guard (try? await self.checkActive()) != nil else { return .failed("Service is no longer active") }
                return .uncertain
            }
        })
    }
}
