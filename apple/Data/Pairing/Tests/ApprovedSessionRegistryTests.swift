import Foundation
import Testing
@testable import AlondraPairing

@MainActor private final class SessionProbe {
    let mac: String
    var stopped = false
    init(_ approved: ApprovedServicePairing) { mac = approved.macID }
}

@MainActor private final class PublicationGate {
    var entered = false
    var waiter: CheckedContinuation<Void, Never>?
    var release: CheckedContinuation<Void, Never>?
    func pause() async {
        entered = true; waiter?.resume(); waiter = nil
        await withCheckedContinuation { release = $0 }
    }
    func waitUntilEntered() async {
        if !entered { await withCheckedContinuation { waiter = $0 } }
    }
    func resume() { release?.resume(); release = nil }
}

private func sessionPairing(_ mac: String, owner: String = "owner-a", key: UInt8 = 1) -> ApprovedServicePairing {
    ApprovedServicePairing(relayURL: URL(string: "https://relay.example")!, macID: mac,
        deviceID: "phone-" + mac, ownerSub: owner, macSigningKey: "pin-" + mac,
        macAgreementKey: "agreement-" + mac, signingPrivateKey: Data([key]),
        agreementPrivateKey: Data([key + 1]), approvedAtMs: 100)
}

@Test @MainActor func selectingTwoMacsRetainsSessionsAndRejectsAnotherAccount() async throws {
    let a = sessionPairing("a"), b = sessionPairing("b"), outsider = sessionPairing("c", owner: "owner-b")
    var created: [SessionProbe] = []
    var snapshots: [ApprovedPairingID?] = []
    var cleared = 0
    let registry = ApprovedSessionRegistry<SessionProbe>(make: { let value = SessionProbe($0); created.append(value); return value },
        stop: { $0.stopped = true }, publish: { snapshot in { snapshots.append(snapshot.selectedID) } }, clearPlatform: { cleared += 1 })
    try await registry.reconcile([a, b, outsider], verifiedSubject: "owner-a", preferred: ApprovedPairingID(a))
    let first = try #require(registry.snapshot.selected?.session)
    #expect(created.count == 2)
    #expect(!created.contains { $0.mac == "c" })
    try await registry.select(ApprovedPairingID(b), verifiedSubject: "owner-a")
    try await registry.select(ApprovedPairingID(a), verifiedSubject: "owner-a")
    #expect(registry.snapshot.selected?.session === first)
    #expect(!created.contains { $0.stopped })
    #expect(cleared == 0)
    await #expect(throws: (any Error).self) { try await registry.select(ApprovedPairingID(outsider), verifiedSubject: "owner-b") }
    #expect(snapshots == [ApprovedPairingID(a), ApprovedPairingID(b), ApprovedPairingID(a)])
    try await registry.reconcile([a, b, outsider], verifiedSubject: "owner-b", preferred: ApprovedPairingID(outsider))
    let previousStopped = created.prefix(2).allSatisfy { $0.stopped }
    #expect(previousStopped)
    #expect(registry.snapshot.entries.count == 1)
    #expect(registry.snapshot.selected?.approved.ownerSub == "owner-b")
    #expect(cleared == 1)
}

@Test @MainActor func logoutDuringSessionPublicationCannotResurrectOldSessions() async throws {
    let gate = PublicationGate()
    var made: [SessionProbe] = []
    var blocked = true
    var committed: [String] = []
    let registry = ApprovedSessionRegistry<SessionProbe>(make: { let s = SessionProbe($0); made.append(s); return s },
        stop: { $0.stopped = true }, publish: { snapshot in
            if blocked { await gate.pause() }
            return { committed.append(snapshot.selected?.approved.macID ?? "none") }
        }, clearPlatform: {})
    let old = Task { try await registry.reconcile([sessionPairing("a")], verifiedSubject: "owner-a", preferred: nil) }
    await gate.waitUntilEntered()
    try await registry.clear()
    blocked = false
    try await registry.reconcile([sessionPairing("b", owner: "owner-b")], verifiedSubject: "owner-b", preferred: nil)
    gate.resume()
    await #expect(throws: CancellationError.self) { try await old.value }
    #expect(committed == ["b"])
    #expect(made[0].stopped)
    #expect(!made[1].stopped)
    #expect(registry.snapshot.selected?.approved.ownerSub == "owner-b")
}

@Test @MainActor func changedKeysCannotReuseLiveSessionAndCleanupStillStopsOnPlatformFailure() async throws {
    enum Failure: Error, Equatable { case cleanup }
    let a = sessionPairing("a")
    var made: [SessionProbe] = []
    let registry = ApprovedSessionRegistry<SessionProbe>(make: { let s = SessionProbe($0); made.append(s); return s },
        stop: { $0.stopped = true }, publish: { _ in {} }, clearPlatform: { throw Failure.cleanup })
    try await registry.reconcile([a], verifiedSubject: "owner-a", preferred: nil)
    await #expect(throws: PairingStoreError.identityConflict) {
        try await registry.reconcile([sessionPairing("a", key: 4)], verifiedSubject: "owner-a", preferred: nil)
    }
    #expect(registry.snapshot.selected?.session === made[0])
    #expect(!made[0].stopped)
    await #expect(throws: Failure.cleanup) { try await registry.clear() }
    #expect(made[0].stopped)
    #expect(registry.snapshot.entries.isEmpty)
}

private func localPairing(_ mac: String) -> ApprovedServicePairing {
    ApprovedServicePairing(relayURL: nil, macID: mac, deviceID: "phone-" + mac, ownerSub: nil,
        macSigningKey: "pin-" + mac, macAgreementKey: "agreement-" + mac, signingPrivateKey: Data([1]),
        agreementPrivateKey: Data([2]), approvedAtMs: 100)
}

@Test @MainActor func withoutAnAccountOnlyLocalPairingsRunAndSurviveSigningIn() async throws {
    let local = localPairing("home"), relay = sessionPairing("remote")
    var created: [SessionProbe] = []
    let registry = ApprovedSessionRegistry<SessionProbe>(make: { let value = SessionProbe($0); created.append(value); return value },
        stop: { $0.stopped = true }, publish: { _ in {} }, clearPlatform: {})
    try await registry.reconcile([local, relay], verifiedSubject: nil, preferred: nil)
    #expect(registry.snapshot.entries.map(\.approved.macID) == ["home"])
    try await registry.select(ApprovedPairingID(local), verifiedSubject: nil)
    await #expect(throws: (any Error).self) { try await registry.select(ApprovedPairingID(relay), verifiedSubject: nil) }
    try await registry.reconcile([local, relay], verifiedSubject: "owner-a", preferred: ApprovedPairingID(relay))
    #expect(Set(registry.snapshot.entries.map(\.approved.macID)) == ["home", "remote"])
    #expect(created.count == 2)
    #expect(created.allSatisfy { !$0.stopped })
}


@Test @MainActor func logoutDuringAsyncSessionHydrationCannotPrepareOrPublishOldCapabilities() async throws {
    let gate = PublicationGate()
    var made: [SessionProbe] = []
    var prepared: [String] = []
    var committed: [String] = []
    let registry = ApprovedSessionRegistry<SessionProbe>(make: { approved in
        let session = SessionProbe(approved)
        made.append(session)
        if approved.ownerSub == "owner-a" { await gate.pause() }
        return session
    }, stop: { $0.stopped = true }, publish: { snapshot in
        let mac = snapshot.selected?.approved.macID ?? "none"
        prepared.append(mac)
        return { committed.append(mac) }
    }, clearPlatform: {})
    let old = Task { try await registry.reconcile([sessionPairing("a")], verifiedSubject: "owner-a", preferred: nil) }
    await gate.waitUntilEntered()
    try await registry.clear()
    try await registry.reconcile([sessionPairing("b", owner: "owner-b")], verifiedSubject: "owner-b", preferred: nil)
    gate.resume()
    await #expect(throws: CancellationError.self) { try await old.value }
    #expect(prepared == ["b"], "old credentials never enter platform preparation after hydration")
    #expect(committed == ["b"])
    #expect(made.count == 2)
    #expect(made[0].stopped)
    #expect(!made[1].stopped)
    #expect(registry.snapshot.selected?.approved.ownerSub == "owner-b")
    try await registry.clear()
    #expect(made[1].stopped)
}
