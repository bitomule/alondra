import AlondraAPIClient
import AlondraNotificationPayload
import CryptoKit
import Foundation
import Security
import Synchronization
import Testing
@testable import AlondraNotificationActions

private final class MemoryDecryptionItem: NotificationDecryptionItemStorage {
    struct State: Sendable {
        var bytes: Data?
        var readError: NotificationDecryptionError?
        var writeError: NotificationDecryptionError?
    }
    let state: Mutex<State>
    init(_ bytes: Data? = nil) { state = Mutex(State(bytes: bytes)) }
    func read() throws -> Data? { try state.withLock { if let error = $0.readError { throw error }; return $0.bytes } }
    func write(_ data: Data) throws { try state.withLock { if let error = $0.writeError { throw error }; $0.bytes = data } }
    func clear() throws { state.withLock { $0.bytes = nil } }
    var bytes: Data? { state.withLock { $0.bytes } }
}

private struct PushFixture {
    let signer: P256.Signing.PrivateKey
    let recipient: P256.KeyAgreement.PrivateKey
    let context: NotificationDecryptionContext
    let binding: NotificationPayload.Binding

    init(_ n: UInt8, owner: String = "owner-a") throws {
        signer = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: n, count: 32))
        recipient = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: n + 10, count: 32))
        binding = try #require(NotificationPayload.Binding(Data(repeating: n, count: 32).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")))
        context = NotificationDecryptionContext(macID: "mac-\(n)", deviceID: "phone-\(n)", binding: binding.value,
            macSigningKey: signer.publicKey.rawRepresentation, agreementPrivateKey: recipient.rawRepresentation,
            scope: NotificationBindingScope(relayURL: URL(string: "https://relay.example")!, ownerSub: owner,
                macID: "mac-\(n)", deviceID: "phone-\(n)", credentialRevision: "revision-\(n)"))
    }

    func wire(outerBinding: String? = nil, innerBinding: String? = nil, route: String? = nil,
              sender: String? = nil, recipientID: String? = nil, expiry: UInt64 = 2000) throws -> Data {
        let body = try JSONSerialization.data(withJSONObject: [
            "push_binding": innerBinding ?? binding.value,
            "payload": ["aps": ["alert": ["title": context.macID, "body": "Synthetic text"]],
                        "alondra": ["kind": "question", "id": 42]]
        ])
        let envelope = try EnvelopeV1.seal(route: route ?? context.macID, sender: sender ?? context.macID,
            recipient: recipientID ?? context.deviceID, requestID: "push-test", kind: "push",
            expiresAtMilliseconds: expiry, plaintext: body, senderKey: signer, recipientKey: recipient.publicKey)
        return try JSONSerialization.data(withJSONObject: ["alondra": [
            "envelope": JSONSerialization.jsonObject(with: JSONEncoder().encode(envelope)),
            "push_binding": outerBinding ?? binding.value, "kind": "report", "id": 999
        ]])
    }
}

@Test func legacyContextReadIsNonmutatingAndVerifiedUpgradeRetainsEveryMac() throws {
    let a = try PushFixture(1)
    let b = try PushFixture(2)
    let old = NotificationDecryptionContext(macID: a.context.macID, deviceID: a.context.deviceID,
        binding: a.binding.value, macSigningKey: a.context.macSigningKey,
        agreementPrivateKey: a.context.agreementPrivateKey)
    var object = try #require(try JSONSerialization.jsonObject(with: JSONEncoder().encode(old)) as? [String: Any])
    object.removeValue(forKey: "scope") // exact pre-collection schema
    let legacy = try JSONSerialization.data(withJSONObject: object)
    let item = MemoryDecryptionItem(legacy)
    let store = NotificationDecryptionStore(item: item)
    #expect(try store.load(binding: a.binding)?.agreementPrivateKey == a.context.agreementPrivateKey)
    #expect(item.bytes == legacy)
    try store.save(b.context)
    try store.save(a.context)
    let restarted = NotificationDecryptionStore(item: item)
    #expect(try restarted.load(binding: a.binding)?.scope?.ownerSub == "owner-a")
    #expect(try restarted.load(binding: a.binding)?.macSigningKey == a.context.macSigningKey)
    #expect(try restarted.load(binding: a.binding)?.agreementPrivateKey == a.context.agreementPrivateKey)
    #expect(try restarted.load(binding: b.binding)?.agreementPrivateKey == b.context.agreementPrivateKey)
}

@Test func encryptedNotificationSelectsOnlyItsOwnRetainedContext() throws {
    let a = try PushFixture(1)
    let b = try PushFixture(2, owner: "owner-b")
    let store = NotificationDecryptionStore(item: MemoryDecryptionItem())
    try store.save(a.context)
    try store.save(b.context)
    for fixture in [a, b] {
        let wire = try fixture.wire()
        let hint = try ServiceNotificationDecoder.bindingHint(wire)
        let context = try #require(try store.load(binding: hint))
        let clear = try ServiceNotificationDecoder.decrypt(wire, context: context, nowMilliseconds: 1000)
        let payload = try #require(try JSONSerialization.jsonObject(with: clear) as? [AnyHashable: Any])
        #expect(NotificationPayload.BoundItem(userInfo: payload)?.item.inboxID == "q42")
        #expect(NotificationPayload.BoundItem(userInfo: payload)?.binding == fixture.binding)
        let aps = try #require(payload["aps"] as? [String: Any])
        #expect((aps["alert"] as? [String: String])?["title"] == fixture.context.macID)
    }
    let altered = try a.wire(outerBinding: b.binding.value)
    let selected = try #require(try store.load(binding: ServiceNotificationDecoder.bindingHint(altered)))
    #expect(throws: (any Error).self) {
        try ServiceNotificationDecoder.decrypt(altered, context: selected, nowMilliseconds: 1000)
    }
    #expect(try store.load(binding: PushFixture(3).binding) == nil)
}

@Test(arguments: ["route", "sender", "recipient", "inner-binding", "expired", "missing-hint"])
func untrustedRoutingCannotProducePlaintextOrActions(mutation: String) throws {
    let a = try PushFixture(1)
    var wire = try a.wire(innerBinding: mutation == "inner-binding" ? PushFixture(2).binding.value : nil,
        route: mutation == "route" ? "mac-other" : nil,
        sender: mutation == "sender" ? "mac-other" : nil,
        recipientID: mutation == "recipient" ? "phone-other" : nil,
        expiry: mutation == "expired" ? 999 : 2000)
    if mutation == "missing-hint" {
        var object = try #require(try JSONSerialization.jsonObject(with: wire) as? [String: [String: Any]])
        object["alondra"]?.removeValue(forKey: "push_binding")
        wire = try JSONSerialization.data(withJSONObject: object)
        #expect(throws: (any Error).self) { try ServiceNotificationDecoder.bindingHint(wire) }
    }
    #expect(throws: (any Error).self) { try ServiceNotificationDecoder.decrypt(wire, context: a.context, nowMilliseconds: 1000) }
}

@Test func reusedBindingCannotReplaceAnotherAccountOrPrivateKey() throws {
    let a = try PushFixture(1)
    let item = MemoryDecryptionItem()
    let store = NotificationDecryptionStore(item: item)
    try store.save(a.context)
    let before = item.bytes
    for changed in [try PushFixture(1, owner: "owner-b").context,
                    NotificationDecryptionContext(macID: a.context.macID, deviceID: a.context.deviceID,
                        binding: a.binding.value, macSigningKey: a.context.macSigningKey,
                        agreementPrivateKey: Data(repeating: 9, count: 32), scope: a.context.scope)] {
        #expect(throws: NotificationDecryptionError.identityConflict) { try store.save(changed) }
        #expect(item.bytes == before)
    }
}

@Test func revokingOneContextKeepsOthersButLogoutClearsAll() throws {
    let a = try PushFixture(1)
    let b = try PushFixture(2)
    let item = MemoryDecryptionItem()
    let store = NotificationDecryptionStore(item: item)
    try store.save(a.context)
    try store.save(b.context)
    try store.remove(binding: a.binding)
    #expect(try store.load(binding: a.binding) == nil)
    #expect(try store.load(binding: b.binding)?.agreementPrivateKey == b.context.agreementPrivateKey)
    try store.clear()
    #expect(item.bytes == nil)
    #expect(try store.load(binding: b.binding) == nil)
}

@Test(arguments: ["locked", "corrupt", "future", "write-failure", "duplicate"])
func failedContextMigrationDoesNotDeletePreviousKeys(failure: String) throws {
    let a = try PushFixture(1)
    let old = try JSONEncoder().encode(a.context)
    let original: Data
    switch failure {
    case "corrupt": original = Data("broken".utf8)
    case "future": original = Data(#"{"version":99,"contexts":[]}"#.utf8)
    case "duplicate":
        let context = try JSONSerialization.jsonObject(with: old)
        original = try JSONSerialization.data(withJSONObject: ["version": 1, "contexts": [context, context]])
    default: original = old
    }
    let item = MemoryDecryptionItem(original)
    item.state.withLock {
        if failure == "locked" { $0.readError = .keychain(errSecInteractionNotAllowed) }
        if failure == "write-failure" { $0.writeError = .keychain(errSecNotAvailable) }
    }
    let store = NotificationDecryptionStore(item: item)
    #expect(throws: (any Error).self) { try store.save(PushFixture(2).context) }
    #expect(item.bytes == original)
}

@Test func authorizedContextSnapshotRetainsTwoMacsAndPrunesPreviousAccountAtomically() throws {
    let a = try PushFixture(1), b = try PushFixture(2), other = try PushFixture(3, owner: "owner-b")
    let item = MemoryDecryptionItem()
    let store = NotificationDecryptionStore(item: item)
    try store.replaceAuthorizedContexts([a.context, b.context])
    #expect(try store.load(binding: a.binding)?.agreementPrivateKey == a.context.agreementPrivateKey)
    #expect(try store.load(binding: b.binding)?.agreementPrivateKey == b.context.agreementPrivateKey)
    let before = item.bytes
    item.state.withLock { $0.writeError = .keychain(errSecNotAvailable) }
    #expect(throws: (any Error).self) { try store.replaceAuthorizedContexts([other.context]) }
    #expect(item.bytes == before)
    item.state.withLock { $0.writeError = nil }
    try store.replaceAuthorizedContexts([other.context])
    #expect(try store.load(binding: a.binding) == nil)
    #expect(try store.load(binding: b.binding) == nil)
    #expect(try store.load(binding: other.binding)?.scope?.ownerSub == "owner-b")
}
