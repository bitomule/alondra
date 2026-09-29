import AlondraNotificationPayload
import AlondraContracts
import Foundation
import XCTest

final class NotificationPayloadTests: XCTestCase {
    func testQuestionPushOffersTheServerOptionsByID() throws {
        let push = try payload("""
        {"aps":{"category":"ALONDRA_QUESTION","mutable-content":1},"alondra":{
          "protocol_version":2,"kind":"question","item_id":"interaction:q-42","interaction_id":"q-42",
          "data_epoch":"epoch","expected_revision":7,
          "push_binding":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "options":[{"id":"approve","label":" Approve "},{"id":"changes","label":"Request changes","text_input":"why"},
                     {"id":"approve","label":"Again"},{"id":"blank","label":" "},{"id":"later","label":"Later"}],
          "reply_option_id":"changes"}}
        """)
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: push))
        let question = try XCTUnwrap(NotificationPayload.Question(userInfo: push))

        XCTAssertEqual(item.inboxID, "interaction:q-42")
        XCTAssertEqual(item.interactionID, "q-42")
        XCTAssertEqual(item.epoch, "epoch")
        XCTAssertEqual(item.expectedRevision, 7)
        XCTAssertEqual(question.options.map(\.id), ["approve", "changes", "later"])
        XCTAssertEqual(question.options.map(\.label), ["Approve", "Request changes", "Later"])
        XCTAssertEqual(question.replyOption?.id, "changes")
        let binding = try XCTUnwrap(NotificationPayload.Binding(userInfo: push))
        XCTAssertEqual(NotificationPayload.Category.questionID(question.id, binding: binding), "ALONDRA_Q_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA_q-42")
        XCTAssertEqual(NotificationPayload.Action.option(question.options[1].id), "OPT:changes")
    }

    func testOnlyV2PushesWithAnItemAreRouted() throws {
        let information = try payload("""
        {"aps":{"category":"ALONDRA_READ"},"alondra":{"protocol_version":2,"kind":"information","item_id":"interaction:i-8","interaction_id":"i-8"}}
        """)
        let v1 = try payload("""
        {"aps":{"category":"ALONDRA_QUESTION"},"alondra":{"kind":"question","id":9,"options":["Allow","Deny"]}}
        """)

        XCTAssertNil(NotificationPayload.Question(userInfo: information))
        XCTAssertEqual(NotificationPayload.Item(userInfo: information)?.inboxID, "interaction:i-8")
        XCTAssertNil(NotificationPayload.Item(userInfo: v1), "A v1 push names nothing the v2 inbox has")
        let binding = try XCTUnwrap(NotificationPayload.Binding("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
        XCTAssertEqual(NotificationPayload.Category.questionID(from: NotificationPayload.Category.questionID("q-9", binding: binding))?.id, "q-9")
        XCTAssertNil(NotificationPayload.Category.questionID(from: "ALONDRA_Q_9"))
        XCTAssertNil(NotificationPayload.Category.questionID(from: "ALONDRA_QUESTION"))
    }

    func testBindingRejectsMissingMalformedAndStaleContext() throws {
        let valid = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        let item: [String: Any] = ["protocol_version": 2, "kind": "question", "item_id": "interaction:q-1"]
        XCTAssertNil(NotificationPayload.BoundItem(userInfo: ["alondra": item]))
        XCTAssertNil(NotificationPayload.Binding(valid + "="))
        XCTAssertNil(NotificationPayload.Binding(String(repeating: "!", count: 43)))

        let first = try XCTUnwrap(NotificationPayload.Binding(valid))
        let second = try XCTUnwrap(NotificationPayload.Binding("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"))
        let bound = try XCTUnwrap(NotificationPayload.BoundItem(userInfo: ["alondra": item.merging(["push_binding": valid]) { $1 }]))
        XCTAssertTrue(bound.matches(first))
        XCTAssertFalse(bound.matches(second))
        XCTAssertFalse(bound.matches(nil))
        let oldID = NotificationPayload.Category.questionID("q-42", binding: first)
        let newID = NotificationPayload.Category.questionID("q-42", binding: second)
        XCTAssertNotEqual(oldID, newID)
        XCTAssertEqual(NotificationPayload.Category.questionID(from: oldID)?.binding, first)
        XCTAssertEqual(NotificationPayload.Category.questionID(from: newID)?.binding, second)
        let withUnderscores = try XCTUnwrap(NotificationPayload.Binding(
            Data(repeating: 0xff, count: 32).base64EncodedString()
                .replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_")
                .replacingOccurrences(of: "=", with: "")
        ))
        XCTAssertEqual(NotificationPayload.Category.questionID(from: NotificationPayload.Category.questionID("q-42", binding: withUnderscores))?.binding, withUnderscores)
    }

    func testPushRegistrationSendsBackendBindingField() throws {
        let request = PushRegistration(
            token: "apns-token", environment: .sandbox,
            quietStart: nil, quietEnd: nil,
            pushBinding: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        )
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertEqual(object["push_binding"] as? String, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
    }

    func testGlobalQuestionCategoryRetentionHandlesLateOldPushAndBoundsGrowth() throws {
        let a = try XCTUnwrap(NotificationPayload.Binding("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
        let b = try XCTUnwrap(NotificationPayload.Binding("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"))
        let b1 = NotificationPayload.Category.questionID("1", binding: b)
        let b2 = NotificationPayload.Category.questionID("2", binding: b)
        let aLate = NotificationPayload.Category.questionID("1", binding: a)
        let first = NotificationPayload.Category.retainedCategoryIDs(from: [], previousOrder: [], incoming: b1, limit: 20)
        let second = NotificationPayload.Category.retainedCategoryIDs(
            from: [b1], previousOrder: first.dynamicOrder, incoming: b2, limit: 20
        )
        let delayed = NotificationPayload.Category.retainedCategoryIDs(
            from: [b1, b2, "ALONDRA_Q_CUSTOM", "ANOTHER_APP_CATEGORY"],
            previousOrder: second.dynamicOrder, incoming: aLate, limit: 20
        )
        XCTAssertTrue(delayed.identifiers.isSuperset(of: [b1, b2, aLate, "ALONDRA_Q_CUSTOM", "ANOTHER_APP_CATEGORY"]))

        let many = (3...24).map { NotificationPayload.Category.questionID(String($0), binding: b) }
        let bounded = NotificationPayload.Category.retainedCategoryIDs(
            from: [b1, b2, aLate, "ALONDRA_Q_CUSTOM"] + many,
            previousOrder: delayed.dynamicOrder + many,
            incoming: nil, limit: 20
        )
        XCTAssertEqual(bounded.dynamicOrder.count, 20)
        XCTAssertTrue(bounded.identifiers.contains(try XCTUnwrap(many.last)))
        XCTAssertTrue(bounded.identifiers.contains("ALONDRA_Q_CUSTOM"))
        XCTAssertFalse(bounded.identifiers.contains(b1))
        let delayedAgain = NotificationPayload.Category.retainedCategoryIDs(
            from: Array(bounded.identifiers), previousOrder: bounded.dynamicOrder,
            incoming: aLate, limit: 20
        )
        XCTAssertEqual(delayedAgain.dynamicOrder.count, 20)
        XCTAssertTrue(delayedAgain.identifiers.contains(aLate))
        XCTAssertTrue(delayedAgain.identifiers.contains("ALONDRA_Q_CUSTOM"))
    }

    private func payload(_ json: String) throws -> [AnyHashable: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }

    func testEveryPushTheDaemonSendsIsRouted() throws {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: env["TEST_SRCDIR"] ?? "").appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        let data = try Data(contentsOf: root.appendingPathComponent("fixtures/client-v2/push-payloads.json"))
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let cases = try XCTUnwrap(fixture["cases"] as? [[String: Any]])
        let binding = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        XCTAssertEqual(cases.count, 5)
        for entry in cases {
            var push = try XCTUnwrap(entry["payload"] as? [String: Any])
            var alondra = try XCTUnwrap(push["alondra"] as? [String: Any])
            alondra["push_binding"] = binding
            push["alondra"] = alondra
            let bound = try XCTUnwrap(NotificationPayload.BoundItem(userInfo: push), "\(entry["interaction"] ?? "")")
            XCTAssertEqual(bound.item.inboxID, alondra["item_id"] as? String)
            XCTAssertEqual(bound.item.kind.rawValue, alondra["kind"] as? String)
            XCTAssertEqual(bound.item.epoch, fixture["data_epoch"] as? String)
        }
    }

    func testTheDaemonsQuestionPushesOfferTheirOptionsAsButtons() throws {
        let fixture = try daemonPushes()
        for name in ["question", "action_request"] {
            let push = try XCTUnwrap(fixture[name])
            let question = try XCTUnwrap(NotificationPayload.Question(userInfo: push), name)
            XCTAssertEqual((push["aps"] as? [String: Any])?["category"] as? String, NotificationPayload.Category.question)
            XCTAssertEqual((push["aps"] as? [String: Any])?["mutable-content"] as? Int, 1)
            XCTAssertEqual(NotificationPayload.Item(userInfo: push)?.expectedRevision, 1)
            XCTAssertEqual(question.options.map(\.id), ["yes", "changes"])
            XCTAssertEqual(question.options.map(\.label), ["Yes", "Request changes"])
            XCTAssertEqual(question.replyOption?.id, "changes")
        }
        let information = try XCTUnwrap(fixture["information"])
        XCTAssertEqual((information["aps"] as? [String: Any])?["category"] as? String, NotificationPayload.Category.read)
        XCTAssertNil(NotificationPayload.Question(userInfo: information))
        XCTAssertNil((try XCTUnwrap(fixture["delivery_text"])["aps"] as? [String: Any])?["category"])
    }

    private func daemonPushes() throws -> [String: [String: Any]] {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: env["TEST_SRCDIR"] ?? "").appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        let data = try Data(contentsOf: root.appendingPathComponent("fixtures/client-v2/push-payloads.json"))
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let cases = try XCTUnwrap(fixture["cases"] as? [[String: Any]])
        return Dictionary(uniqueKeysWithValues: try cases.map { (try XCTUnwrap($0["interaction"] as? String), try XCTUnwrap($0["payload"] as? [String: Any])) })
    }
}
