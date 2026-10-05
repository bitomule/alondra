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

struct DeliveredNotificationSyncTests {
    private let mac = NotificationPayload.Binding(String(repeating: "B", count: 42) + "A")!
    private let otherMac = NotificationPayload.Binding(String(repeating: "C", count: 42) + "A")!

    private func delivered(_ identifier: String, card: String?, binding: NotificationPayload.Binding?) -> DeliveredNotificationSync.Delivered {
        var alondra: [String: Any] = [:]
        if let card { alondra["card"] = card }
        if let binding { alondra["push_binding"] = binding.value }
        return .init(identifier: identifier, userInfo: alondra.isEmpty ? [:] : ["alondra": alondra])
    }

    private var center: [DeliveredNotificationSync.Delivered] {
        [delivered("n-answered", card: "t-1-question-1", binding: mac),
         delivered("n-waiting", card: "t-1-info-1", binding: mac),
         delivered("n-other-mac", card: "t-9-question-1", binding: otherMac),
         delivered("n-notice", card: nil, binding: nil),
         delivered("n-old-push", card: "t-2-question-1", binding: nil)]
    }

    @Test func theWholeQueueRemovesEveryCardOfThatMacThatNoLongerWaits() {
        let stale = DeliveredNotificationSync.stale(center, binding: mac, queue: .open(["t-1-info-1"]))
        #expect(stale == ["n-answered"])
    }

    @Test func aRetractedListRemovesOnlyTheCardsItNames() {
        let stale = DeliveredNotificationSync.stale(center, binding: mac, queue: .retracted(["t-1-question-1", "t-7-gone"]))
        #expect(stale == ["n-answered"])
    }

    @Test func anEmptyQueueClearsThisMacOnlyAndNeverTheAppsOwnNotices() {
        let stale = DeliveredNotificationSync.stale(center, binding: mac, queue: .open([]))
        #expect(stale == ["n-answered", "n-waiting"], "another Mac's cards and card-less notices stay")
    }

    @Test func aRetractionNeedsItsKindABindingAndAQueue() {
        let base: [String: Any] = ["push_binding": mac.value, "kind": "retract"]
        func retraction(_ extra: [String: Any], aps: [String: Any] = ["badge": 2]) -> DeliveredNotificationSync.Retraction? {
            .init(userInfo: ["aps": aps, "alondra": base.merging(extra) { $1 }])
        }
        #expect(retraction(["open": ["a", "b"]]) == .init(userInfo: ["aps": ["badge": 2],
            "alondra": ["push_binding": mac.value, "kind": "retract", "open": ["b", "a"]]]))
        #expect(retraction(["open": ["a"]])?.queue == .open(["a"]))
        #expect(retraction(["open": ["a"]])?.badge == 2)
        #expect(retraction(["retracted": ["c"]])?.queue == .retracted(["c"]))
        #expect(retraction(["open": ["a"]], aps: [:])?.badge == nil)
        #expect(retraction([:]) == nil, "no queue says nothing")
        #expect(retraction(["kind": "question", "open": ["a"]]) == nil, "an alert is not a retraction")
        #expect(retraction(["push_binding": "short", "open": ["a"]]) == nil)
    }

    /// fixtures/push/retract.json is sealed by the daemon's push_retract and wrapped by the relay
    /// as a background push; both Rust crates check it still matches what they send.
    @Test func theAppOpensTheDaemonSealedRetraction() throws {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: env["TEST_SRCDIR"] ?? "").appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        let data = try Data(contentsOf: root.appendingPathComponent("fixtures/push/retract.json"))
        let fixture = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let apns = try #require(fixture["apns"] as? [String: Any])
        let binding = try #require((apns["alondra"] as? [String: Any])?["push_binding"] as? String)
        func decode(_ value: Any?) throws -> Data {
            let text = try #require(value as? String)
            let standard = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            return try #require(Data(base64Encoded: standard + String(repeating: "=", count: (4 - standard.count % 4) % 4)))
        }
        let macID = try #require(fixture["mac_id"] as? String)
        let deviceID = try #require(fixture["device_id"] as? String)
        let scope = NotificationBindingScope(relayURL: try #require(URL(string: "https://relay.example")),
            ownerSub: "owner", macID: macID, deviceID: deviceID, credentialRevision: "revision")
        let store = NotificationDecryptionStore(item: MemoryItem())
        try store.replaceAuthorizedContexts([NotificationDecryptionContext(macID: macID, deviceID: deviceID,
            binding: binding, macSigningKey: try decode(fixture["mac_signing_public_key"]),
            agreementPrivateKey: try decode(fixture["device_agreement_private_key"]), scope: scope)])

        let payload = try #require(NotificationDecoder.open(userInfo: apns, store: store))
        let retraction = try #require(DeliveredNotificationSync.Retraction(userInfo: payload))

        #expect(retraction.binding.value == binding)
        #expect(retraction.queue == .open(["t-1-info-1"]))
        #expect(retraction.badge == 1)
        #expect((apns["aps"] as? [String: Any])?.keys.sorted() == ["content-available"], "APNs sees no alert, badge or sound")
    }
}
