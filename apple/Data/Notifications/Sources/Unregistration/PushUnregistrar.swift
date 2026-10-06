import AlondraTransport
import Foundation

/// Drains the ledger one pass at a time. A binding the app is using again is never
/// sent to the relay: it is live, so it only leaves the queue.
public actor PushUnregistrar {
    public typealias Unregister = @Sendable (PushUnregistrationLedger.Entry) async -> RelayPushUnregistration.Outcome

    public nonisolated let ledger: PushUnregistrationLedger
    private let unregister: Unregister
    private var draining: Task<Void, Never>?

    public init(ledger: PushUnregistrationLedger, unregister: @escaping Unregister) {
        self.ledger = ledger
        self.unregister = unregister
    }

    public init(ledger: PushUnregistrationLedger) {
        let relay = RelayPushUnregistration()
        self.init(ledger: ledger) { await relay.unregister(binding: $0.pushBinding, relay: $0.relayURL) }
    }

    public func drain(live: Set<NotificationPayload.Binding>) async {
        let previous = draining
        let pass = Task { await previous?.value; await run(live: Set(live.map(\.value))) }
        draining = pass
        await pass.value
    }

    private func run(live: Set<String>) async {
        guard let pending = try? ledger.pending() else { return }
        for entry in pending {
            if !live.contains(entry.pushBinding), await unregister(entry) == .retry { continue }
            try? ledger.remove([entry])
        }
    }

    /// Bindings the notification extension saw without keys came from an ended session.
    /// Without a known relay they stay put until one is known.
    public nonisolated static func promote(_ orphans: OrphanBindingStore, into ledger: PushUnregistrationLedger,
                                           live: Set<NotificationPayload.Binding>, relay: URL?) throws {
        let found = try orphans.load()
        guard !found.isEmpty else { return }
        let relay = relay ?? (try? ledger.pending())?.last?.relayURL
        let stale = found.filter { !live.contains($0) }
        if let relay {
            try ledger.enqueue(stale.map { PushUnregistrationLedger.Entry(relayURL: relay, pushBinding: $0.value) })
            try orphans.remove(found)
        } else {
            try orphans.remove(found.filter { live.contains($0) })
        }
    }
}
