import Dispatch
import Foundation
import Security
import Synchronization
import Testing
@testable import AlondraServicePairing

private final class MemoryPairingItem: PairingItemStorage {
    struct State: Sendable {
        var bytes: Data?
        var readError: PairingStoreError?
        var writeError: PairingStoreError?
    }
    let state: Mutex<State>
    init(_ bytes: Data? = nil) { state = Mutex(State(bytes: bytes)) }
    func read() throws -> Data? {
        try state.withLock {
            if let error = $0.readError { throw error }
            return $0.bytes
        }
    }
    func write(_ data: Data) throws {
        try state.withLock {
            if let error = $0.writeError { throw error }
            $0.bytes = data
        }
    }
    func delete() throws { state.withLock { $0.bytes = nil } }
    var bytes: Data? { state.withLock { $0.bytes } }
}

private let legacyBytes = Data(#"{"relayURL":"https://relay.example","macID":"mac-a","deviceID":"phone-a","ownerSub":"owner-a","macSigningKey":"pinned-signing-a","macAgreementKey":"pinned-agreement-a","signingPrivateKey":"AQIDBA==","agreementPrivateKey":"BQYHCA==","approvedAtMs":1800000000000}"#.utf8)

private func binding(mac: String = "mac-b", device: String = "phone-b", owner: String = "owner-a",
                     relay: String = "https://relay.example", signing: String = "pinned-signing-b",
                     privateKey: Data = Data([11, 12, 13, 14]), approved: UInt64 = 1_800_000_000_100) -> ApprovedServicePairing {
    ApprovedServicePairing(relayURL: URL(string: relay)!, macID: mac, deviceID: device,
        ownerSub: owner, macSigningKey: signing, macAgreementKey: "pinned-agreement-b",
        signingPrivateKey: privateKey, agreementPrivateKey: Data([15, 16, 17, 18]), approvedAtMs: approved)
}

@Test func legacyReadDoesNotRewriteSecretsAndNextApprovalPreservesThem() throws {
    let item = MemoryPairingItem(legacyBytes)
    let store = KeychainApprovedPairingStore(item: item)
    let old = try #require(try store.load())
    #expect(old.signingPrivateKey == Data([1, 2, 3, 4]))
    #expect(old.agreementPrivateKey == Data([5, 6, 7, 8]))
    #expect(old.approvedAtMs == 1_800_000_000_000)
    #expect(item.bytes == legacyBytes)

    try store.save(binding())
    let restarted = KeychainApprovedPairingStore(item: item)
    let collection = try restarted.loadCollection()
    #expect(collection.pairings.count == 2)
    #expect(try restarted.load()?.macID == "mac-b")
    let retained = try #require(collection.pairings.first { $0.macID == "mac-a" })
    #expect(retained.signingPrivateKey == Data([1, 2, 3, 4]))
    #expect(retained.agreementPrivateKey == Data([5, 6, 7, 8]))
    #expect(retained.macSigningKey == "pinned-signing-a")
    #expect(retained.macAgreementKey == "pinned-agreement-a")
    #expect(retained.deviceID == "phone-a")
    #expect(retained.ownerSub == "owner-a")
    #expect(retained.approvedAtMs == 1_800_000_000_000)
}

@Test func accountAndRelayLookupsNeverMixBindings() throws {
    var collection = ApprovedPairingCollection()
    try collection.upsert(binding())
    try collection.upsert(binding(owner: "owner-b", signing: "another-account-pin"))
    try collection.upsert(binding(relay: "https://other-relay.example", signing: "another-relay-pin"))
    let accountA = collection.pairings(ownerSub: "owner-a", relayURL: URL(string: "https://relay.example")!)
    let accountB = collection.pairings(ownerSub: "owner-b", relayURL: URL(string: "https://relay.example")!)
    #expect(accountA.map(\.macSigningKey) == ["pinned-signing-b"])
    #expect(accountB.map(\.macSigningKey) == ["another-account-pin"])
    #expect(collection.pairings(ownerSub: "unknown", relayURL: URL(string: "https://relay.example")!).isEmpty)
    #expect(collection.pairings.count == 3)
}

@Test func reapprovalRetainsOtherDevicesAndIdempotentSavePreservesJournalRevision() throws {
    let store = KeychainApprovedPairingStore(item: MemoryPairingItem())
    try store.save(binding())
    try store.save(binding(device: "phone-new", privateKey: Data([21]), approved: 1_800_000_000_200))
    try store.save(binding(approved: 1_800_000_000_999))
    let collection = try store.loadCollection()
    #expect(collection.pairings.count == 2)
    #expect(collection.selected?.deviceID == "phone-b")
    #expect(collection.selected?.approvedAtMs == 1_800_000_000_100)
    #expect(collection.pairings.first { $0.deviceID == "phone-new" }?.signingPrivateKey == Data([21]))
}

@Test(arguments: ["same-device-pin", "new-device-pin", "same-device-secret"])
func conflictingApprovalCannotOverwriteStoredKeys(kind: String) throws {
    let item = MemoryPairingItem()
    let store = KeychainApprovedPairingStore(item: item)
    try store.save(binding())
    let before = item.bytes
    let candidate = binding(device: kind == "new-device-pin" ? "phone-new" : "phone-b",
        signing: kind == "same-device-secret" ? "pinned-signing-b" : "substituted-pin",
        privateKey: kind == "same-device-secret" ? Data([99]) : Data([11, 12, 13, 14]))
    #expect(throws: PairingStoreError.identityConflict) { try store.save(candidate) }
    #expect(item.bytes == before)
}

@Test(arguments: ["locked", "corrupt", "future", "write-failure"])
func persistenceFailureNeverReplacesExistingBytes(kind: String) throws {
    let original: Data = switch kind {
    case "corrupt": Data("not JSON".utf8)
    case "future": Data(#"{"version":999,"pairings":[],"selectedID":null}"#.utf8)
    default: legacyBytes
    }
    let item = MemoryPairingItem(original)
    item.state.withLock {
        if kind == "locked" { $0.readError = .keychain(errSecInteractionNotAllowed) }
        if kind == "write-failure" { $0.writeError = .keychain(errSecNotAvailable) }
    }
    let store = KeychainApprovedPairingStore(item: item)
    #expect(throws: (any Error).self) { try store.save(binding()) }
    #expect(item.bytes == original)
}

@Test(arguments: ["missing-selection", "foreign-owner-selection", "duplicate-id"])
func malformedCollectionFailsClosedWithoutOverwriting(kind: String) throws {
    var collection = ApprovedPairingCollection()
    try collection.upsert(binding())
    var object = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(collection)) as? [String: Any])
    if kind == "missing-selection" { object.removeValue(forKey: "selectedID") }
    if kind == "foreign-owner-selection" {
        var selection = try #require(object["selectedID"] as? [String: Any])
        selection["ownerSub"] = "someone-else"
        object["selectedID"] = selection
    }
    if kind == "duplicate-id" {
        let records = try #require(object["pairings"] as? [[String: Any]])
        object["pairings"] = records + records
    }
    let original = try JSONSerialization.data(withJSONObject: object)
    let item = MemoryPairingItem(original)
    let store = KeychainApprovedPairingStore(item: item)
    #expect(throws: PairingStoreError.invalidCollection) { try store.load() }
    #expect(throws: PairingStoreError.invalidCollection) { try store.save(binding(mac: "mac-other")) }
    #expect(item.bytes == original)
}

@Test func twoStoreInstancesShareTransactionSoConcurrentApprovalsAreNotLost() throws {
    let item = MemoryPairingItem(legacyBytes)
    let first = KeychainApprovedPairingStore(item: item)
    let second = KeychainApprovedPairingStore(item: item)
    let failures = Mutex<[String]>([])
    DispatchQueue.concurrentPerform(iterations: 32) { index in
        do { try (index.isMultiple(of: 2) ? first : second).save(binding(mac: "mac-\(index)")) }
        catch { failures.withLock { $0.append(String(describing: error)) } }
    }
    #expect(failures.withLock { $0.isEmpty })
    let saved = try first.loadCollection()
    #expect(saved.pairings.count == 33)
    #expect(Set(saved.pairings.map(\.macID)) == Set(["mac-a"] + (0..<32).map { "mac-\($0)" }))
    #expect(saved.pairings.first { $0.macID == "mac-a" }?.signingPrivateKey == Data([1, 2, 3, 4]))
}

@Test func explicitLogoutDeletesAllBindingsInsteadOfExposingAnotherSelection() throws {
    let item = MemoryPairingItem(legacyBytes)
    let store = KeychainApprovedPairingStore(item: item)
    try store.save(binding(owner: "owner-b"))
    try store.delete()
    #expect(item.bytes == nil)
    #expect(try store.load() == nil)
    #expect(try store.loadCollection().pairings.isEmpty)
}

@Test func selectingStoredMacPersistsWithoutLosingAnotherAccountsKeys() throws {
    let item = MemoryPairingItem(legacyBytes)
    let store = KeychainApprovedPairingStore(item: item)
    let a = try #require(try store.load())
    let b = binding()
    let other = binding(mac: "other", owner: "owner-b")
    try store.save(b)
    try store.save(other)
    try store.select(ApprovedPairingID(a))
    let restarted = KeychainApprovedPairingStore(item: item)
    #expect(try restarted.load()?.macID == a.macID)
    #expect(try restarted.loadCollection().pairings.count == 3)
    #expect(try restarted.loadCollection().pairings.first { $0.ownerSub == "owner-b" }?.signingPrivateKey == other.signingPrivateKey)
    let before = item.bytes
    #expect(throws: PairingStoreError.invalidCollection) { try store.select(ApprovedPairingID(binding(mac: "unknown"))) }
    #expect(item.bytes == before)
}
