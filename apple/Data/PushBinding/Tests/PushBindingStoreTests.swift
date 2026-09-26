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
        let first = try XCTUnwrap(store.binding(server: a, deviceID: "same-id", credentialRevision: "key-1"))
        let relaunched = PushBindingStore(defaults: defaults, randomBytes: { nil })
        XCTAssertEqual(relaunched.binding(server: a, deviceID: "same-id", credentialRevision: "key-1"), first)
        let changedServer = try XCTUnwrap(store.binding(server: b, deviceID: "same-id", credentialRevision: "key-1"))
        XCTAssertNotEqual(changedServer, first)
        let changedCredential = try XCTUnwrap(store.binding(server: b, deviceID: "same-id", credentialRevision: "key-2"))
        XCTAssertNotEqual(changedCredential, changedServer)
        XCTAssertNotEqual(store.binding(server: a, deviceID: "same-id", credentialRevision: "key-1"), first)
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
