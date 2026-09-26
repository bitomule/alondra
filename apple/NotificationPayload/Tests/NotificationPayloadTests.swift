import AlondraNotificationPayload
import AlondraContracts
import Foundation
import XCTest

final class NotificationPayloadTests: XCTestCase {
    func testDaemonQuestionPushRoutesToDynamicActions() throws {
        let push = try payload("""
        {"aps":{"category":"ALONDRA_QUESTION","mutable-content":1},"alondra":{
          "kind":"question","id":42,"origin":"ask_user_question",
          "push_binding":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "options":[" Approve ","Request changes","Approve"," ","Later"]}}
        """)
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: push))
        let question = try XCTUnwrap(NotificationPayload.Question(userInfo: push))

        XCTAssertEqual(item.inboxID, "q42")
        XCTAssertEqual(question.options, ["Approve", "Request changes", "Later"])
        let binding = try XCTUnwrap(NotificationPayload.Binding(userInfo: push))
        XCTAssertEqual(NotificationPayload.Category.questionID(question.id, binding: binding), "ALONDRA_Q_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA_42")
        XCTAssertEqual(NotificationPayload.Action.option(question.options[1]), "OPT:Request changes")
    }

    func testPermissionAndReportPushesKeepFixedCategories() throws {
        let permission = try payload("""
        {"aps":{"category":"ALONDRA_PERMISSION","mutable-content":1},"alondra":{
          "kind":"question","id":9,"origin":"permission","options":["Allow","Deny"]}}
        """)
        let report = try payload("""
        {"aps":{"category":"ALONDRA_READ"},"alondra":{"kind":"report","id":8}}
        """)

        XCTAssertNil(NotificationPayload.Question(userInfo: permission))
        XCTAssertEqual(NotificationPayload.Item(userInfo: permission)?.inboxID, "q9")
        XCTAssertNil(NotificationPayload.Question(userInfo: report))
        XCTAssertEqual(NotificationPayload.Item(userInfo: report)?.inboxID, "r8")
        let binding = try XCTUnwrap(NotificationPayload.Binding("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
        XCTAssertEqual(NotificationPayload.Category.questionID(from: NotificationPayload.Category.questionID(9, binding: binding))?.id, 9)
        XCTAssertNil(NotificationPayload.Category.questionID(from: "ALONDRA_Q_9"))
        XCTAssertNil(NotificationPayload.Category.questionID(from: "ALONDRA_QUESTION"))
    }

    func testBindingRejectsMissingMalformedAndStaleContext() throws {
        let valid = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        let missing: [AnyHashable: Any] = ["alondra": ["kind": "question", "id": 1]]
        XCTAssertNil(NotificationPayload.BoundItem(userInfo: missing))
        XCTAssertNil(NotificationPayload.Binding(valid + "="))
        XCTAssertNil(NotificationPayload.Binding(String(repeating: "!", count: 43)))

        let first = try XCTUnwrap(NotificationPayload.Binding(valid))
        let second = try XCTUnwrap(NotificationPayload.Binding("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"))
        let bound = try XCTUnwrap(NotificationPayload.BoundItem(userInfo: ["alondra": ["kind": "question", "id": 1, "push_binding": valid]]))
        XCTAssertTrue(bound.matches(first))
        XCTAssertFalse(bound.matches(second))
        XCTAssertFalse(bound.matches(nil))
        let oldID = NotificationPayload.Category.questionID(42, binding: first)
        let newID = NotificationPayload.Category.questionID(42, binding: second)
        XCTAssertNotEqual(oldID, newID)
        XCTAssertEqual(NotificationPayload.Category.questionID(from: oldID)?.binding, first)
        XCTAssertEqual(NotificationPayload.Category.questionID(from: newID)?.binding, second)
        let withUnderscores = try XCTUnwrap(NotificationPayload.Binding(
            Data(repeating: 0xff, count: 32).base64EncodedString()
                .replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_")
                .replacingOccurrences(of: "=", with: "")
        ))
        XCTAssertEqual(NotificationPayload.Category.questionID(from: NotificationPayload.Category.questionID(42, binding: withUnderscores))?.binding, withUnderscores)
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
        let b1 = NotificationPayload.Category.questionID(1, binding: b)
        let b2 = NotificationPayload.Category.questionID(2, binding: b)
        let aLate = NotificationPayload.Category.questionID(1, binding: a)
        let first = NotificationPayload.Category.retainedCategoryIDs(from: [], previousOrder: [], incoming: b1, limit: 20)
        let second = NotificationPayload.Category.retainedCategoryIDs(
            from: [b1], previousOrder: first.dynamicOrder, incoming: b2, limit: 20
        )
        let delayed = NotificationPayload.Category.retainedCategoryIDs(
            from: [b1, b2, "ALONDRA_Q_CUSTOM", "ANOTHER_APP_CATEGORY"],
            previousOrder: second.dynamicOrder, incoming: aLate, limit: 20
        )
        XCTAssertTrue(delayed.identifiers.isSuperset(of: [b1, b2, aLate, "ALONDRA_Q_CUSTOM", "ANOTHER_APP_CATEGORY"]))

        let many = (3...24).map { NotificationPayload.Category.questionID(UInt64($0), binding: b) }
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
}
