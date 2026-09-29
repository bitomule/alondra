import AlondraDomain
import AlondraNotificationPayload
import Foundation

/// Resolves authenticated notifications to an immutable approved session. Selection is
/// presentation state; changing it never retargets an already captured command.
@MainActor
public final class NotificationRoutingCoordinator {
    public struct Entry {
        public let client: ServiceNotificationClient
        public let context: NotificationDecryptionContext
        public init(client: ServiceNotificationClient, context: NotificationDecryptionContext) {
            self.client = client; self.context = context
        }
    }
    public struct Authorized {
        public let item: NotificationPayload.Item
        public let question: NotificationPayload.Question?
        public let binding: NotificationPayload.Binding
        fileprivate let capability: UUID
    }
    private struct Live {
        let entry: Entry
        let capability: UUID
    }
    private var entries: [NotificationPayload.Binding: Live] = [:]
    public private(set) var selected: NotificationPayload.Binding?

    public init() {}

    public func install(_ proposed: [Entry], selected: NotificationPayload.Binding?) throws {
        guard Set(proposed.map { $0.client.binding }).count == proposed.count,
              Set(proposed.map { $0.client.identity }).count == proposed.count,
              Set(proposed.compactMap { $0.context.scope?.ownerSub }).count <= 1 else {
            throw NotificationDecryptionError.invalidCollection
        }
        var next: [NotificationPayload.Binding: Live] = [:]
        for entry in proposed {
            guard let scope = entry.context.scope, scope.isValid,
                  scope.macID == entry.context.macID, scope.deviceID == entry.context.deviceID,
                  scope.credentialRevision == entry.client.identity,
                  entry.client.binding.value == entry.context.binding else {
                throw NotificationDecryptionError.invalidCollection
            }
            let previous = entries[entry.client.binding]
            let retained = previous?.entry.client.identity == entry.client.identity && previous?.entry.context == entry.context
            next[entry.client.binding] = Live(entry: entry, capability: retained ? previous!.capability : UUID())
        }
        guard selected == nil || next[selected!] != nil else { throw NotificationDecryptionError.invalidCollection }
        entries = next
        self.selected = selected
    }

    public func clear() { entries = [:]; selected = nil }

    /// The outer binding is only a lookup hint. No authority escapes before signature,
    /// recipient, route, expiry and inner binding have all been checked by the decoder.
    public func authorize(_ raw: Data) throws -> Authorized {
        let hint = try ServiceNotificationDecoder.bindingHint(raw)
        guard let live = entries[hint] else { throw NotificationDecryptionError.invalidPayload }
        let plaintext = try ServiceNotificationDecoder.decrypt(raw, context: live.entry.context)
        guard let payload = try JSONSerialization.jsonObject(with: plaintext) as? [AnyHashable: Any],
              let bound = NotificationPayload.BoundItem(userInfo: payload), bound.binding == hint else {
            throw NotificationDecryptionError.invalidPayload
        }
        return Authorized(item: bound.item, question: NotificationPayload.Question(userInfo: payload), binding: hint,
                          capability: live.capability)
    }

    public func isValid(_ authorized: Authorized) -> Bool {
        entries[authorized.binding]?.capability == authorized.capability
    }

    public func activate(_ authorized: Authorized,
                         select: @MainActor (NotificationPayload.Binding) async throws -> Void) async throws {
        guard isValid(authorized) else { throw CancellationError() }
        try await select(authorized.binding)
        guard !Task.isCancelled, isValid(authorized), selected == authorized.binding else { throw CancellationError() }
    }

    public func submit(_ authorized: Authorized, actionID: String, text: String?) async -> NotificationActionResult? {
        guard isValid(authorized), let client = entries[authorized.binding]?.entry.client,
              let action = NotificationAction(item: authorized.item, question: authorized.question,
                                              action: actionID, text: text) else { return nil }
        let result = await client.submit(action, authorized.item)
        guard isValid(authorized), !Task.isCancelled else { return nil }
        return result
    }
}
