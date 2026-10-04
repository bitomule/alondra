import Foundation
import Synchronization
import Testing
@testable import AlondraNotifications

private final class MemoryItem: NotificationDecryptionItemStorage {
    let bytes = Mutex<Data?>(nil)
    func read() throws -> Data? { bytes.withLock { $0 } }
    func write(_ data: Data) throws { bytes.withLock { $0 = data } }
    func clear() throws { bytes.withLock { $0 = nil } }
}

/// fixtures/push/encrypted.json is sealed by the daemon's `seal_push` and wrapped by the
/// relay's APNs payload; both Rust crates check the fixture still matches what they send.
struct EncryptedPushFixtureTests {
    private let fixture: [String: Any]

    init() throws {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: env["TEST_SRCDIR"] ?? "").appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        let data = try Data(contentsOf: root.appendingPathComponent("fixtures/push/encrypted.json"))
        fixture = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private var apns: [AnyHashable: Any] { fixture["apns"] as? [String: Any] ?? [:] }

    private func context(binding: String) throws -> NotificationDecryptionContext {
        let macID = try #require(fixture["mac_id"] as? String)
        let deviceID = try #require(fixture["device_id"] as? String)
        let scope = NotificationBindingScope(relayURL: try #require(URL(string: "https://relay.example")),
            ownerSub: "owner", macID: macID, deviceID: deviceID, credentialRevision: "revision")
        return NotificationDecryptionContext(macID: macID, deviceID: deviceID, binding: binding,
            macSigningKey: try decode(fixture["mac_signing_public_key"]),
            agreementPrivateKey: try decode(fixture["device_agreement_private_key"]), scope: scope)
    }

    private func decode(_ value: Any?) throws -> Data {
        let text = try #require(value as? String)
        let standard = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        return try #require(Data(base64Encoded: standard + String(repeating: "=", count: (4 - standard.count % 4) % 4)))
    }

    private var binding: String {
        get throws { try #require((apns["alondra"] as? [String: Any])?["push_binding"] as? String) }
    }

    @Test func extensionOpensTheDaemonSealedPush() throws {
        let store = NotificationDecryptionStore(item: MemoryItem())
        try store.replaceAuthorizedContexts([context(binding: binding)])

        let payload = try #require(NotificationDecoder.open(userInfo: apns, store: store))

        let item = try #require(NotificationPayload.Item(userInfo: payload))
        let sent = try #require((fixture["payload"] as? [String: Any])?["alondra"] as? [String: Any])
        #expect(item.kind == .question)
        #expect(item.card == sent["card"] as? String)
        #expect(item.actions.map(\.label) == ["★ Yes", "Request changes", "Responder"])
        #expect(item.actions.map(\.text) == [false, true, true])
        #expect(NotificationPayload.Binding(userInfo: payload)?.value == (try binding))
        #expect((payload["aps"] as? [String: Any])?["sound"] == nil, "with_sound false strips the sound")
        let alert = NotificationAlert(aps: try #require(payload["aps"] as? [String: Any]))
        #expect(alert.title == "Merge the question parser?")
        #expect(alert.body == "The lexer split is ready and its tests pass.",
                "the lock screen shows what the card needs, then why")
    }

    @Test func aPushWithoutTextFallsBackToTheExtensionsOwnLine() {
        #expect(NotificationAlert(aps: ["alert": ["title": "Alondra"]]).body == nil)
        #expect(NotificationAlert(aps: ["alert": ["title": " ", "body": "  "]]) == NotificationAlert(aps: [:]))
        #expect(NotificationAlert(aps: [:]).title == "Alondra")
    }

    @Test func aPushForAReplacedPairingCannotBeOpened() throws {
        let store = NotificationDecryptionStore(item: MemoryItem())
        let current = Data(repeating: 9, count: 32).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        try store.replaceAuthorizedContexts([context(binding: current)])

        #expect(NotificationDecoder.open(userInfo: apns, store: store) == nil)
    }
}
