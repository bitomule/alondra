import AlondraContracts
import AlondraLoopbackSession
import AlondraNotificationActions
import AlondraNotificationPayload
import AlondraWorkspace
import CryptoKit
import Foundation
import XCTest

/// A push is short-lived and may be tapped long after the item changed. The action must act on
/// the item as the Mac serves it now, under a new command ID each time.
final class NotificationActionSubmissionTests: XCTestCase {
    private var directory: URL!

    override func setUp() {
        directory = FileManager.default.temporaryDirectory.appending(path: "notification-\(UUID().uuidString)")
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
    }

    func testAnOldPushAnswersTheItemAsItIsNowWithANewCommandIDEachTime() async throws {
        let daemon = mac(lookup: lookup(pending: true, revision: 12))
        let client = try await notificationClient(daemon)
        let (item, action) = try pushAnswer(expectedRevision: 7)

        let first = await client.submit(action, item)
        let second = await client.submit(action, item)

        XCTAssertEqual(first, .sent(.recorded(recorded)))
        XCTAssertEqual(second, .sent(.recorded(recorded)))
        let commands = try daemon.requests.filter { $0.target == "/interactions/q-1/answers" }.map {
            try XCTUnwrap(JSONSerialization.jsonObject(with: $0.body) as? [String: Any])
        }
        XCTAssertEqual(commands.count, 2)
        XCTAssertEqual(commands.map { $0["expected_revision"] as? Int }, [12, 12], "The push's revision 7 is never used")
        XCTAssertEqual(commands.map { $0["data_epoch"] as? String }, ["epoch-1", "epoch-1"])
        let ids = commands.compactMap { $0["command_id"] as? String }
        XCTAssertEqual(Set(ids).count, 2, "Each tap is a new command")
        XCTAssertFalse(ids.contains { $0.hasPrefix("notification-") })
        XCTAssertEqual(commands.first?["option_id"] as? String, "approve")
        XCTAssertNil(commands.first?["kind"])
        XCTAssertNil(commands.first?["interaction_id"])
        let reads = daemon.requests.map(\.target).filter { $0.hasPrefix("/inbox") || $0.hasPrefix("/interactions") }
        XCTAssertEqual(reads, ["/inbox/interaction%3Aq-1", "/interactions/q-1", "/interactions/q-1/answers",
                               "/inbox/interaction%3Aq-1", "/interactions/q-1", "/interactions/q-1/answers"])
    }

    func testAnAlreadyAnsweredItemIsNoLongerPendingAndNothingIsSent() async throws {
        let daemon = mac(lookup: lookup(pending: false, revision: 12))
        let client = try await notificationClient(daemon)
        let (item, action) = try pushAnswer(expectedRevision: 7)

        let result = await client.submit(action, item)

        XCTAssertEqual(result, .noLongerPending)
        XCTAssertFalse(daemon.requests.contains { $0.target == "/interactions/q-1/answers" })
    }

    func testAnItemTheMacNoLongerHasIsNoLongerPending() async throws {
        let daemon = mac(lookup: (404, Data(#"{"code":"not_found","message":"no such inbox item"}"#.utf8)))
        let client = try await notificationClient(daemon)
        let (item, action) = try pushAnswer(expectedRevision: 7)

        let result = await client.submit(action, item)

        XCTAssertEqual(result, .noLongerPending)
        XCTAssertFalse(daemon.requests.contains { $0.target == "/interactions/q-1/answers" })
    }

    func testAPushFromAnotherDataEpochNeverActsOnANewItemWithTheSameID() async throws {
        let daemon = mac(lookup: lookup(pending: true, revision: 3, epoch: "epoch-2"))
        let client = try await notificationClient(daemon)
        let (item, action) = try pushAnswer(expectedRevision: 7)

        let result = await client.submit(action, item)

        XCTAssertEqual(result, .noLongerPending)
        XCTAssertFalse(daemon.requests.contains { $0.target == "/interactions/q-1/answers" })
    }

    // MARK: - Fixture

    private let recorded = DomainV2.CommandOutcome(revision: 13, changed: true, effects: [], createdIds: [])

    private func lookup(pending: Bool, revision: Int, epoch: String = "epoch-1") -> (Int, Data) {
        (200, Data(#"""
        {"protocol_version":2,"data_epoch":"\#(epoch)","revision":\#(revision),"root_id":"interaction:q-1","item":{"id":"interaction:q-1","kind":"question","subject":{"kind":"interaction","interaction_id":"q-1"},"title":"Deploy?","revision":\#(revision),"order":3,"pending":\#(pending),"children":[],"available_actions":[{"kind":"answer_question","enabled":\#(pending),"expected_revision":\#(revision)}]}}
        """#.utf8))
    }

    private let interaction = Data(#"""
    {"protocol_version":2,"data_epoch":"epoch-1","revision":12,"truncated":false,"item":{"attachments":[],"id":"q-1","scope":{"kind":"task","task_id":"t-1"},"author":{"kind":"agent","agent_id":"a-1"},"recipient":{"kind":"user"},"body":"Deploy?","blocking":true,"payload":{"kind":"question","options":[{"id":"approve","label":"Approve","inputs":[]}]},"revision":12,"order":3}}
    """#.utf8)

    private func mac(lookup: (Int, Data)) -> StubDaemon {
        StubDaemon { [interaction] request in
            switch request.target {
            case "/pairing": .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/pair": .status(200, Data(#"{"device_id":"device"}"#.utf8))
            case "/inbox/interaction%3Aq-1": .status(lookup.0, lookup.1)
            case "/interactions/q-1": .status(200, interaction)
            case "/interactions/q-1/answers": .status(200, Data(#"{"revision":13,"changed":true,"effects":[],"created_ids":[]}"#.utf8))
            default: .status(404, Data(#"{"code":"not_found","message":"unexpected"}"#.utf8))
            }
        }
    }

    private func notificationClient(_ daemon: StubDaemon) async throws -> ServiceNotificationClient {
        let identity = LoopbackIdentityStore(directory: directory, makeKey: { .software(P256.Signing.PrivateKey()) })
        let session = LoopbackSession(identity: identity, deviceName: "Mac", urlSession: daemon.urlSession())
        let services = try await CloudAppServices.local(session, identity: identity)
        let binding = try XCTUnwrap(NotificationPayload.Binding(Data(repeating: 7, count: 32).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")))
        return await services.notificationClient(identity: "identity", binding: binding)
    }

    private func pushAnswer(expectedRevision: Int) throws -> (NotificationPayload.Item, NotificationAction) {
        let userInfo = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(#"""
        {"alondra":{"protocol_version":2,"kind":"question","item_id":"interaction:q-1","interaction_id":"q-1","data_epoch":"epoch-1","expected_revision":\#(expectedRevision),"options":[{"id":"approve","label":"Approve"}]}}
        """#.utf8)) as? [AnyHashable: Any])
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: userInfo))
        let action = try XCTUnwrap(NotificationAction(item: item, question: NotificationPayload.Question(userInfo: userInfo),
                                                      action: NotificationPayload.Action.option("approve"), text: nil))
        return (item, action)
    }
}
