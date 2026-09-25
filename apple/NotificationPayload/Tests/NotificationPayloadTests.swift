import AlondraNotificationPayload
import Foundation
import XCTest

final class NotificationPayloadTests: XCTestCase {
    func testDaemonQuestionPushRoutesToDynamicActions() throws {
        let push = try payload("""
        {"aps":{"category":"ALONDRA_QUESTION","mutable-content":1},"alondra":{
          "kind":"question","id":42,"origin":"ask_user_question",
          "options":[" Approve ","Request changes","Approve"," ","Later"]}}
        """)
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: push))
        let question = try XCTUnwrap(NotificationPayload.Question(userInfo: push))

        XCTAssertEqual(item.inboxID, "q42")
        XCTAssertEqual(question.options, ["Approve", "Request changes", "Later"])
        XCTAssertEqual(NotificationPayload.Category.questionID(question.id), "ALONDRA_Q_42")
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
        XCTAssertEqual(NotificationPayload.Category.questionID(from: "ALONDRA_Q_9"), 9)
        XCTAssertNil(NotificationPayload.Category.questionID(from: "ALONDRA_QUESTION"))
    }

    private func payload(_ json: String) throws -> [AnyHashable: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }
}
