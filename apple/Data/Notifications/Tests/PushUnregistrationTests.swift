import AlondraTransport
import Foundation
import Synchronization
import Testing
@testable import AlondraNotifications

private final class MemoryOrphanItem: NotificationDecryptionItemStorage {
    let bytes = Mutex<Data?>(nil)
    func read() throws -> Data? { bytes.withLock { $0 } }
    func write(_ data: Data) throws { bytes.withLock { $0 = data } }
    func clear() throws { bytes.withLock { $0 = nil } }
}

private actor RelayCalls {
    var sent: [String] = []
    func record(_ binding: String) { sent.append(binding) }
}

private func temporaryFile() -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("alondra-unregistration-\(UUID().uuidString)", isDirectory: true)
        .appendingPathComponent("push-unregistration.json")
}

private func temporaryLedger() -> PushUnregistrationLedger { PushUnregistrationLedger(file: temporaryFile()) }

private func binding(_ n: UInt8) -> NotificationPayload.Binding {
    NotificationPayload.Binding(Data(repeating: n, count: 32).base64EncodedString()
        .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: ""))!
}

private let relay = URL(string: "https://relay.example")!

private func entry(_ n: UInt8) -> PushUnregistrationLedger.Entry {
    PushUnregistrationLedger.Entry(relayURL: relay, pushBinding: binding(n).value)
}

@Suite struct PushUnregistrationTests {
    @Test func ledgerSurvivesNewInstancesAndKeepsOneEntryPerBinding() throws {
        let file = temporaryFile()
        let ledger = PushUnregistrationLedger(file: file)
        try ledger.enqueue([entry(1), entry(2), entry(1)])
        try ledger.enqueue([entry(2)])
        let reopened = PushUnregistrationLedger(file: file)
        #expect(try reopened.pending() == [entry(1), entry(2)])
        try reopened.remove([entry(1)])
        #expect(try ledger.pending() == [entry(2)])
    }

    @Test(arguments: [
        (RelayPushUnregistration.Outcome.done, false),
        (.drop, false),
        (.retry, true),
    ])
    func drainKeepsOnlyEntriesToRetry(outcome: RelayPushUnregistration.Outcome, kept: Bool) async throws {
        let ledger = temporaryLedger()
        try ledger.enqueue([entry(1)])
        let calls = RelayCalls()
        let unregistrar = PushUnregistrar(ledger: ledger) { entry in
            await calls.record(entry.pushBinding)
            return outcome
        }
        await unregistrar.drain(live: [])
        #expect(await calls.sent == [binding(1).value])
        #expect(try ledger.pending() == (kept ? [entry(1)] : []))
    }

    @Test func drainNeverUnregistersTheLiveBinding() async throws {
        let ledger = temporaryLedger()
        try ledger.enqueue([entry(1), entry(2)])
        let calls = RelayCalls()
        let unregistrar = PushUnregistrar(ledger: ledger) { entry in
            await calls.record(entry.pushBinding)
            return .done
        }
        await unregistrar.drain(live: [binding(1)])
        #expect(await calls.sent == [binding(2).value])
        #expect(try ledger.pending().isEmpty)
    }

    @Test func orphanPromotionSkipsLiveBindingsAndWaitsForARelay() throws {
        let orphans = OrphanBindingStore(item: MemoryOrphanItem())
        let ledger = temporaryLedger()
        try orphans.append(binding(1))
        try orphans.append(binding(2))
        try PushUnregistrar.promote(orphans, into: ledger, live: [binding(1)], relay: nil)
        #expect(try ledger.pending().isEmpty)
        #expect(try orphans.load() == [binding(2)])
        try PushUnregistrar.promote(orphans, into: ledger, live: [], relay: relay)
        #expect(try ledger.pending() == [entry(2)])
        #expect(try orphans.load().isEmpty)
    }

    @Test func orphanPromotionFallsBackToTheLastQueuedRelay() throws {
        let orphans = OrphanBindingStore(item: MemoryOrphanItem())
        let ledger = temporaryLedger()
        try ledger.enqueue([entry(1)])
        try orphans.append(binding(2))
        try PushUnregistrar.promote(orphans, into: ledger, live: [], relay: nil)
        #expect(try ledger.pending() == [entry(1), entry(2)])
    }

    @Test func orphanListIsBoundedAndDeduplicated() throws {
        let orphans = OrphanBindingStore(item: MemoryOrphanItem())
        for n in 0..<40 { try orphans.append(binding(UInt8(n))) }
        try orphans.append(binding(39))
        let found = try orphans.load()
        #expect(found.count == OrphanBindingStore.limit)
        #expect(found.first == binding(8))
        #expect(found.last == binding(39))
    }

    @Test @MainActor func retiringBindingsQueuesThemBeforeClearing() throws {
        let suite = "alondra-push-retire-\(UUID())"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        var next: UInt8 = 1
        let store = PushBindingStore(defaults: defaults) {
            defer { next += 1 }
            return Data(repeating: next, count: 32)
        }
        let scopes = ["mac-a", "mac-b"].map {
            NotificationBindingScope(relayURL: relay, ownerSub: "owner", macID: $0, deviceID: "phone", credentialRevision: "rev")
        }
        let issued = scopes.compactMap { store.binding(scope: $0) }
        #expect(store.live == Set(issued))
        let ledger = temporaryLedger()
        store.retire(into: ledger)
        #expect(try ledger.pending() == issued.map { PushUnregistrationLedger.Entry(relayURL: relay, pushBinding: $0.value) })
        #expect(store.live.isEmpty)
    }
}
