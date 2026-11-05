import AlondraLiveActivity
import AlondraNotifications
import AlondraTransport
import Foundation
import Testing

/// fixtures/push/activity.json is the body the daemon's `seal_activity` posts to the relay.
private struct Fixture: Decodable {
    struct Body: Decodable { let envelope: Envelope }
    let body: Body
    let macID: String
    let deviceID: String
    let pushBinding: String
    let macSigningPublicKey: String
    let deviceAgreementPrivateKey: String

    enum CodingKeys: String, CodingKey {
        case body, macID = "mac_id", deviceID = "device_id", pushBinding = "push_binding"
        case macSigningPublicKey = "mac_signing_public_key", deviceAgreementPrivateKey = "device_agreement_private_key"
    }

    static func load() throws -> Self {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: env["TEST_SRCDIR"] ?? "").appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        return try JSONDecoder().decode(Self.self, from: Data(contentsOf: root.appendingPathComponent("fixtures/push/activity.json")))
    }

    func context(binding: String? = nil) throws -> NotificationDecryptionContext {
        NotificationDecryptionContext(macID: macID, deviceID: deviceID, binding: binding ?? pushBinding,
                                      macSigningKey: try Self.decoded(macSigningPublicKey),
                                      agreementPrivateKey: try Self.decoded(deviceAgreementPrivateKey))
    }

    private static func decoded(_ base64URL: String) throws -> Data {
        let base64 = base64URL.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        return try #require(Data(base64Encoded: base64.padding(toLength: (base64.count + 3) / 4 * 4, withPad: "=", startingAt: 0)))
    }
}

@Suite struct ActivityEnvelopesTests {
    @Test func opensWhatTheDaemonSeals() throws {
        let fixture = try Fixture.load()
        let reports = ActivityEnvelopes.open([fixture.body.envelope], with: [try fixture.context()])
        #expect(reports == [ActivityReport(macID: fixture.macID, macName: "Mac mini", counts: ActivityCounts(inbox: 3, waiting: 1, working: 2),
                                           measuredAt: Date(timeIntervalSince1970: 1_790_000_000))])
    }

    @Test func aPushAfterTheEnvelopeExpiredStillOpens() throws {
        let fixture = try Fixture.load()
        #expect(Double(fixture.body.envelope.expiresAtMilliseconds) / 1000 < Date(timeIntervalSince1970: 1_800_000_000).timeIntervalSince1970)
        #expect(ActivityEnvelopes.open([fixture.body.envelope], with: [try fixture.context()]).count == 1)
    }

    @Test func anEnvelopeWithoutItsPairingIsLeftOut() throws {
        let fixture = try Fixture.load()
        #expect(ActivityEnvelopes.open([fixture.body.envelope], with: []).isEmpty)
    }

    @Test func aPairingWhoseBindingChangedOpensNothing() throws {
        let fixture = try Fixture.load()
        #expect(ActivityEnvelopes.open([fixture.body.envelope], with: [try fixture.context(binding: "another")]).isEmpty)
    }

    /// The exact `aps.content-state` of a relay push, as ActivityKit hands it to the widget.
    @Test func decodesThePushOfTheRelay() throws {
        let fixture = try Fixture.load()
        let envelope = try JSONSerialization.jsonObject(with: JSONEncoder().encode(fixture.body.envelope))
        let pushed = try JSONSerialization.data(withJSONObject: ["macs": [envelope], "omitted": 1])
        let state = try JSONDecoder().decode(ActivityContentState.self, from: pushed)
        #expect(state.macs == [fixture.body.envelope])
        #expect(state.omitted == 1)
        #expect(state.reports.isEmpty)
        #expect(try JSONDecoder().decode(ActivityContentState.self, from: JSONEncoder().encode(state)) == state)
    }

    @Test func theNewestWordOfEachMacWins() {
        let older = ActivityReport(macID: "mac", macName: nil, counts: .zero, measuredAt: Date(timeIntervalSince1970: 10))
        let newer = ActivityReport(macID: "mac", macName: nil, counts: ActivityCounts(inbox: 1, waiting: 0, working: 0),
                                   measuredAt: Date(timeIntervalSince1970: 20))
        #expect(ActivityContentState(reports: [older]).merged(with: [newer]) == [newer])
        #expect(ActivityContentState(reports: [newer]).merged(with: [older]) == [newer])
    }
}
