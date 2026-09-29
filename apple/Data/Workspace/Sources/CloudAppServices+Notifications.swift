import AlondraAPIClient
import AlondraContracts
import AlondraDomain
import AlondraNotificationActions
import AlondraNotificationPayload
import Foundation

public extension CloudAppServices {
    /// Root derives identity/binding from the approved pairing, persists extension keys,
    /// then installs this immutable client. No presentation store is needed to act.
    func notificationClient(identity: String, binding: NotificationPayload.Binding) -> ServiceNotificationClient {
        ServiceNotificationClient(identity: identity, binding: binding, register: { token, environment, quietStart, quietEnd, revision in
            try await self.checkActive()
            try await self.api.registerServicePush(token: token, environment: environment, binding: binding.value, preferencesRevision: revision, quietStart: quietStart, quietEnd: quietEnd)
            try await self.checkActive()
        }, submit: { action, item in
            await self.submitNotificationAction(action, item: item)
        })
    }
}

extension CloudAppServices {
    /// A push may be tapped long after it arrived. The item is read as it is now and the
    /// command gets a new ID; a push from another data epoch never acts on a new item that
    /// happens to share its ID.
    func submitNotificationAction(_ action: NotificationAction, item: NotificationPayload.Item,
                                  commandID: String = UUID().uuidString.lowercased()) async -> NotificationActionResult {
        guard (try? checkActive()) != nil else { return .sent(.obsolete) }
        let envelope: DomainV2.CommandEnvelope
        do {
            let lookup = try await api.domainInboxItem(item.itemID)
            if let epoch = item.epoch, epoch != lookup.dataEpoch.rawValue { return .noLongerPending }
            let interaction = try await api.domainInteraction(action.interactionID).item
            envelope = try action.envelope(fresh: lookup, interaction: interaction, commandID: commandID)
        } catch NotificationActionError.noLongerPending {
            return .noLongerPending
        } catch let error as DomainV2.DomainError where error.code == DomainV2.ErrorCode.notFound.rawValue {
            return .noLongerPending
        } catch {
            guard (try? checkActive()) != nil else { return .sent(.obsolete) }
            return .sent(.rejected(Self.rejection(error)))
        }
        return .sent(await send(envelope))
    }
}
