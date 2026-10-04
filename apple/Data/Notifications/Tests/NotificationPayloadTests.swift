import AlondraNotifications
import AlondraContract
import Foundation
import XCTest

final class NotificationPayloadTests: XCTestCase {
    func testParsesServerActionIDsAndTextInput() throws {
        let push = try payload(#"{"aps":{"category":"ALONDRA_PUSH","mutable-content":1},"alondra":{"protocol":1,"kind":"question","card":"q-42","data_epoch":"epoch","push_binding":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","actions":[{"id":"ans:q-42@117","option":"approve","label":"Approve"},{"id":"ans:q-42@117","option":"other","label":"Other","text":true,"placeholder":"Tell it what to do"}]}}"#)
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: push))
        let binding = try XCTUnwrap(NotificationPayload.Binding(userInfo: push))

        XCTAssertEqual(item.card, "q-42")
        XCTAssertEqual(item.inboxID, "interaction:q-42")
        XCTAssertEqual(item.epoch, "epoch")
        XCTAssertEqual(item.actions.map(\.id), ["ans:q-42@117", "ans:q-42@117"])
        XCTAssertEqual(item.actions.map(\.option), ["approve", "other"])
        XCTAssertFalse(item.actions[0].text)
        XCTAssertTrue(item.actions[1].text)
        XCTAssertEqual(item.actions[1].placeholder, "Tell it what to do")
        XCTAssertEqual(NotificationPayload.Category.questionID(item.card, binding: binding), "ALONDRA_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA_q-42")
        XCTAssertEqual(NotificationPayload.Action.index("A1"), 1)
        XCTAssertNil(NotificationPayload.Action.index("OPT:approve"))
    }

    func testAPlanPushApprovesOrOpensTheCardToReview() throws {
        let push = try payload(#"{"alondra":{"protocol":1,"kind":"question","card":"hold-1","data_epoch":"e","actions":[{"id":"ans:hold-1@3","option":"approve","label":"Approve"},{"id":"open:hold-1","label":"Review","open":true}]}}"#)
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: push))
        XCTAssertEqual(item.actions.map(\.opens), [false, true])
        XCTAssertFalse(item.opens("A0"))
        XCTAssertTrue(item.opens("A1"))
        XCTAssertFalse(item.opens("A2"))
    }

    func testOnlyViewsPushesWithValidCardsAreRouted() throws {
        let payload = try payload(#"{"alondra":{"protocol":1,"kind":"information","card":"i-8","data_epoch":"e","actions":[]}}"#)
        XCTAssertEqual(NotificationPayload.Item(userInfo: payload)?.card, "i-8")
        XCTAssertNil(NotificationPayload.Item(userInfo: try self.payload(#"{"alondra":{"protocol_version":2,"kind":"question","item_id":"interaction:q-1"}}"#)))
        XCTAssertNil(NotificationPayload.Item(userInfo: try self.payload(#"{"alondra":{"protocol":1,"kind":"question","card":"bad/id","data_epoch":"e","actions":[]}}"#)))
        XCTAssertNil(NotificationPayload.Item(userInfo: try self.payload(#"{"alondra":{"protocol":1,"kind":"question","card":"q-1","data_epoch":"e"}}"#)))
    }

    func testBindingRejectsMissingMalformedAndStaleContext() throws {
        let valid = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        let item: [String: Any] = ["protocol": 1, "kind": "question", "card": "q-1", "data_epoch": "e", "actions": []]
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
        XCTAssertEqual(NotificationPayload.Category.questionID(from: oldID)?.id, "q-42")
        XCTAssertEqual(NotificationPayload.Category.questionID(from: newID)?.binding, second)
        XCTAssertNil(NotificationPayload.Category.questionID(from: "ALONDRA_Q_9"))
    }

    func testPushRegistrationSendsBackendBindingField() throws {
        let request = PushRegistration(token: "apns-token", environment: .sandbox,
            quietStart: nil, quietEnd: nil, pushBinding: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        XCTAssertEqual(object["push_binding"] as? String, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
    }

    func testGlobalCardCategoryRetentionHandlesLateOldPushAndBoundsGrowth() throws {
        let a = try XCTUnwrap(NotificationPayload.Binding("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
        let b = try XCTUnwrap(NotificationPayload.Binding("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"))
        let b1 = NotificationPayload.Category.questionID("1", binding: b)
        let b2 = NotificationPayload.Category.questionID("2", binding: b)
        let aLate = NotificationPayload.Category.questionID("1", binding: a)
        let first = NotificationPayload.Category.retainedCategoryIDs(from: [], previousOrder: [], incoming: b1, limit: 20)
        let second = NotificationPayload.Category.retainedCategoryIDs(from: [b1], previousOrder: first.dynamicOrder, incoming: b2, limit: 20)
        let delayed = NotificationPayload.Category.retainedCategoryIDs(
            from: [b1, b2, "ALONDRA_CUSTOM", "ANOTHER_APP_CATEGORY"], previousOrder: second.dynamicOrder,
            incoming: aLate, limit: 20)
        XCTAssertTrue(delayed.identifiers.isSuperset(of: [b1, b2, aLate, "ALONDRA_CUSTOM", "ANOTHER_APP_CATEGORY"]))

        let many = (3...24).map { NotificationPayload.Category.questionID(String($0), binding: b) }
        let bounded = NotificationPayload.Category.retainedCategoryIDs(
            from: [b1, b2, aLate, "ALONDRA_CUSTOM"] + many, previousOrder: delayed.dynamicOrder + many,
            incoming: nil, limit: 20)
        XCTAssertEqual(bounded.dynamicOrder.count, 20)
        XCTAssertTrue(bounded.identifiers.contains(try XCTUnwrap(many.last)))
        XCTAssertTrue(bounded.identifiers.contains("ALONDRA_CUSTOM"))
        XCTAssertFalse(bounded.identifiers.contains(b1))
    }

    func testEveryDaemonPushMatchesTheViewsFixture() throws {
        let fixture = try fixture()
        let cases = try XCTUnwrap(fixture["cases"] as? [[String: Any]])
        XCTAssertEqual(cases.count, 6)
        let binding = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        for entry in cases {
            var push = try XCTUnwrap(entry["payload"] as? [String: Any])
            var alondra = try XCTUnwrap(push["alondra"] as? [String: Any])
            alondra["push_binding"] = binding
            push["alondra"] = alondra
            let bound = try XCTUnwrap(NotificationPayload.BoundItem(userInfo: push))
            XCTAssertEqual(bound.item.card, alondra["card"] as? String)
            XCTAssertEqual(bound.item.kind.rawValue, alondra["kind"] as? String)
            XCTAssertEqual(bound.item.epoch, fixture["data_epoch"] as? String)
        }
    }

    func testAPermissionPushSaysWhoAsksForWhatAndOffersAllowAndDeny() throws {
        let cases = try XCTUnwrap(try fixture()["cases"] as? [[String: Any]])
        let push = try XCTUnwrap(cases.first { $0["interaction"] as? String == "permission" }?["payload"] as? [String: Any])
        let alert = NotificationAlert(aps: try XCTUnwrap(push["aps"] as? [String: Any]))
        XCTAssertEqual(alert.title, "Allow extracting the waiting-items helper")
        XCTAssertEqual(alert.body, "It rewrites a few files in core views.")
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: push))
        XCTAssertEqual(item.actions.map(\.label), ["Allow", "Deny", "Reply"])
        XCTAssertEqual(item.actions.map(\.option), ["allow", "deny", nil])
        XCTAssertEqual(Set(item.actions.map(\.id)), ["ans:hold-permission-toolu_01JWScmn532U7ZW6ruo7BjpS@1", "reply:hold-permission-toolu_01JWScmn532U7ZW6ruo7BjpS@1"])
        XCTAssertEqual(item.actions.map(\.text), [false, true, true], "Allow answers at once; Deny and the reply take a line of text")
        XCTAssertFalse(item.actions.contains { $0.opens })
    }

    func testEveryPushedCardEndsWithItsReplyAsATextAction() throws {
        let cases = try XCTUnwrap(try fixture()["cases"] as? [[String: Any]])
        for pushed in cases {
            let push = try XCTUnwrap(pushed["payload"] as? [String: Any])
            let item = try XCTUnwrap(NotificationPayload.Item(userInfo: push), "\(pushed["interaction"] ?? "")")
            let reply = try XCTUnwrap(item.actions.last, "\(pushed["interaction"] ?? "")")
            XCTAssertTrue(reply.id.hasPrefix("reply:") || reply.id.hasPrefix("msg:"), reply.id)
            XCTAssertTrue(reply.text, "the reply is the text-input action of the notification")
            XCTAssertNotNil(reply.placeholder)
            XCTAssertLessThanOrEqual(item.actions.count, 4, "a category shows four buttons at most")
        }
    }

    func testThePayloadSaysHowLoudlyAndUnderWhichThreadItArrives() throws {
        let cases = try XCTUnwrap(try fixture()["cases"] as? [[String: Any]])
        func alert(_ interaction: String) throws -> NotificationAlert {
            let push = try XCTUnwrap(cases.first { $0["interaction"] as? String == interaction }?["payload"] as? [String: Any])
            return NotificationAlert(aps: try XCTUnwrap(push["aps"] as? [String: Any]))
        }
        XCTAssertEqual(try alert("action_request").level, .timeSensitive)
        XCTAssertEqual(try alert("question").level, .active)
        XCTAssertEqual(try alert("information").level, .passive)
        XCTAssertEqual(try alert("question").thread, "orchestrator")
        XCTAssertNil(NotificationAlert(aps: ["interruption-level": "critical", "thread-id": ""]).level)
        XCTAssertNil(NotificationAlert(aps: ["thread-id": ""]).thread)
    }

    func testFourButtonTextActionCategoryIsBuiltFromPayload() throws {
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: payload(#"{"alondra":{"protocol":1,"kind":"question","card":"q-1","data_epoch":"e","actions":[{"id":"a","label":"One"},{"id":"b","label":"Two"},{"id":"c","label":"Three","text":true,"placeholder":"Explain"},{"id":"d","label":"Four"}]}}"#)))
        XCTAssertEqual(item.actions.count, 4)
        XCTAssertEqual(item.actions[2].placeholder, "Explain")
        XCTAssertTrue(item.actions[2].text)
    }

    func testRealPRCardCanCarryNotificationActions() throws {
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: payload(#"{"alondra":{"protocol":1,"kind":"pull_request","card":"github.com/bitomule/alondra#154","data_epoch":"e","actions":[{"id":"ask_pr:github.com/bitomule/alondra#154@117","label":"Ask"}]}}"#)))
        XCTAssertEqual(item.card, "github.com/bitomule/alondra#154")
        XCTAssertEqual(item.actions.first?.id, "ask_pr:github.com/bitomule/alondra#154@117")
        XCTAssertFalse(NotificationPayload.Category.isSafe("github.com/../alondra#154"))
    }

    private func payload(_ json: String) throws -> [AnyHashable: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }

    private func fixture() throws -> [String: Any] {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: env["TEST_SRCDIR"] ?? "").appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        let data = try Data(contentsOf: root.appendingPathComponent("fixtures/views/push-payloads.json"))
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }
}
