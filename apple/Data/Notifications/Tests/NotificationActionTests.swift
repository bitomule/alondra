import AlondraNotificationActions
import AlondraCommands
import AlondraNotificationPayload
import Foundation
import XCTest

final class NotificationActionTests: XCTestCase {
    func testDaemonPayloadRoutesOnlyValidActionsToMatchingResource() throws {
        let questionPush = try payload(#"{"aps":{"category":"ALONDRA_QUESTION","mutable-content":1},"alondra":{"kind":"question","id":42,"origin":"ask","options":["Approve","Request changes"]}}"#)
        let reportPush = try payload(#"{"aps":{"category":"ALONDRA_READ","mutable-content":1},"alondra":{"kind":"report","id":8}}"#)
        let question = try XCTUnwrap(NotificationPayload.Item(userInfo: questionPush))
        let report = try XCTUnwrap(NotificationPayload.Item(userInfo: reportPush))

        XCTAssertEqual(NotificationAction(item: question, action: "OPT:Request changes", text: nil), .answer(question: 42, text: "Request changes", option: "Request changes"))
        XCTAssertEqual(NotificationAction(item: question, action: "OPT:Request changes", text: nil)?.commandKind, .answer(42))
        XCTAssertEqual(NotificationAction(item: question, action: "REPLY", text: "  Need logs  "), .answer(question: 42, text: "Need logs", option: nil))
        XCTAssertEqual(NotificationAction(item: report, action: "READ", text: nil), .markRead(report: 8))
        XCTAssertEqual(NotificationAction(item: report, action: "READ", text: nil)?.commandKind, .markRead(8))
        XCTAssertNil(NotificationAction(item: report, action: "ALLOW", text: nil))
        XCTAssertNil(NotificationAction(item: question, action: "READ", text: nil))
        XCTAssertNil(NotificationAction(item: question, action: "REPLY", text: "  "))
    }

    private func payload(_ json: String) throws -> [AnyHashable: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [AnyHashable: Any])
    }
}
