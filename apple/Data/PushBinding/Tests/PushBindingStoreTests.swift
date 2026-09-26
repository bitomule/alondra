import AlondraNotificationPayload
import AlondraPushBinding
import Foundation
import XCTest

final class PushBindingStoreTests: XCTestCase {
    @MainActor
    func testRotatesForServerAndCredentialEvenWithSameDeviceID() throws {
        let suite = "alondra-push-binding-\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        var next: UInt8 = 0
        let store = PushBindingStore(defaults: defaults) {
            defer { next &+= 1 }
            return Data(repeating: next, count: 32)
        }
        let a = try XCTUnwrap(URL(string: "https://a.example"))
        let b = try XCTUnwrap(URL(string: "https://b.example"))
        let first = try XCTUnwrap(store.binding(scope: scope(server: a, revision: "key-1")))
        let relaunched = PushBindingStore(defaults: defaults, randomBytes: { nil })
        XCTAssertEqual(relaunched.binding(scope: scope(server: a, revision: "key-1")), first)
        let changedServer = try XCTUnwrap(store.binding(scope: scope(server: b, revision: "key-1")))
        XCTAssertNotEqual(changedServer, first)
        let changedCredential = try XCTUnwrap(store.binding(scope: scope(server: b, revision: "key-2")))
        XCTAssertNotEqual(changedCredential, changedServer)
        XCTAssertEqual(store.binding(scope: scope(server: a, revision: "key-1")), first)
    }

    private func scope(server: URL = URL(string: "https://relay.example")!, revision: String = "key-1",
                       owner: String = "owner-a", mac: String = "mac-a") -> NotificationBindingScope {
        NotificationBindingScope(relayURL: server, ownerSub: owner, macID: mac,
            deviceID: "same-id", credentialRevision: revision)
    }

    @MainActor
    func testLegacyAdoptionRequiresMatchingCredentialContextAndPreservesOtherAccount() throws {
        let suite = "alondra-binding-migration-\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let legacy = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        let server = "https://relay.example"
        let data = try JSONSerialization.data(withJSONObject: ["context": "21:https://relay.example7:same-id5:key-a", "binding": legacy])
        defaults.set(data, forKey: "alondra.push-binding.v1")
        let store = PushBindingStore(defaults: defaults, randomBytes: { Data(repeating: 1, count: 32) })
        let other = try XCTUnwrap(store.binding(scope: scope(server: URL(string: server)!, revision: "key-b", owner: "owner-b")))
        XCTAssertNotEqual(other.value, legacy)
        let adopted = try XCTUnwrap(store.binding(scope: scope(revision: "key-a")))
        XCTAssertEqual(adopted.value, legacy)
        let relaunched = PushBindingStore(defaults: defaults, randomBytes: { nil })
        XCTAssertEqual(relaunched.binding(scope: scope(revision: "key-a")), adopted)
        XCTAssertEqual(relaunched.binding(scope: scope(revision: "key-b", owner: "owner-b")), other)
        XCTAssertTrue(store.remove(scope: scope(revision: "key-a")))
        XCTAssertNil(relaunched.binding(scope: scope(revision: "key-a")))
        XCTAssertEqual(relaunched.binding(scope: scope(revision: "key-b", owner: "owner-b")), other)
        store.clear()
        XCTAssertNil(defaults.data(forKey: "alondra.push-binding.v1"))
        XCTAssertNil(relaunched.binding(scope: scope(revision: "key-a")))
    }

    @MainActor
    func testBindingCollisionDoesNotAssignAnotherMacOrAccountTheSameLabel() throws {
        let suite = "alondra-binding-collision-\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = PushBindingStore(defaults: defaults, randomBytes: { Data(repeating: 2, count: 32) })
        let original = try XCTUnwrap(store.binding(scope: scope()))
        let before = defaults.data(forKey: "alondra.push-binding.v1")
        XCTAssertNil(store.binding(scope: scope(mac: "mac-b")))
        XCTAssertNil(store.binding(scope: scope(owner: "owner-b")))
        XCTAssertEqual(defaults.data(forKey: "alondra.push-binding.v1"), before)
        XCTAssertEqual(store.binding(scope: scope()), original)
    }

    @MainActor
    func testMalformedOrUnknownCollectionIsNotReplacedByFreshBinding() throws {
        let suite = "alondra-binding-corrupt-\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = PushBindingStore(defaults: defaults, randomBytes: { Data(repeating: 3, count: 32) })
        for data in [Data("broken".utf8), Data(#"{"version":2,"records":[]}"#.utf8)] {
            defaults.set(data, forKey: "alondra.push-binding.v1")
            XCTAssertNil(store.binding(scope: scope()))
            XCTAssertEqual(defaults.data(forKey: "alondra.push-binding.v1"), data)
        }
    }

    func testLateRegistrationCompletionCannotReplaceNewPairingState() throws {
        let a = try XCTUnwrap(NotificationPayload.Binding("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
        let b = try XCTUnwrap(NotificationPayload.Binding("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"))
        let first = PushRegistrationScope(clientIdentity: "https://server#same-id", binding: a, token: "token")
        let rotated = PushRegistrationScope(clientIdentity: "https://server#same-id", binding: b, token: "token")
        XCTAssertFalse(first.acceptsCompletion(current: rotated, cancelled: false))
        XCTAssertFalse(first.acceptsCompletion(current: nil, cancelled: false))
        XCTAssertFalse(first.acceptsCompletion(current: first, cancelled: true))
        XCTAssertTrue(rotated.acceptsCompletion(current: rotated, cancelled: false))
    }
}
