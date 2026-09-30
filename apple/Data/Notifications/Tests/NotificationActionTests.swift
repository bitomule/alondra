import AlondraNotificationActions
import AlondraNotificationPayload
import AlondraViews
import Foundation
import XCTest

final class NotificationActionTests: XCTestCase {
    func testButtonTapPostsRenderedActionAndOptionWithoutReadingInbox() throws {
        let item = try item()
        let action = try XCTUnwrap(NotificationAction(item: item, action: "A0", text: nil, lang: "en"))
        let request = action.request(for: item, commandID: "95de96be-7251-4e27-abf7-1f2292ba8ad2")

        XCTAssertEqual(request.actionId, "ans:i-question@117")
        XCTAssertEqual(request.optionId, "cookies")
        XCTAssertEqual(request.dataEpoch, "epoch")
        XCTAssertEqual(request.client, .ios)
        XCTAssertEqual(request.commandId, "95de96be-7251-4e27-abf7-1f2292ba8ad2")
        XCTAssertNil(request.input.text)
    }

    func testTextButtonPostsItsOpaqueActionOptionAndTypedText() throws {
        let item = try item()
        let action = try XCTUnwrap(NotificationAction(item: item, action: "A1", text: "  Do this  ", lang: "es"))
        let request = action.request(for: item, commandID: "95de96be-7251-4e27-abf7-1f2292ba8ad2")

        XCTAssertEqual(request.actionId, "ans:i-question@117")
        XCTAssertEqual(request.optionId, "other")
        XCTAssertEqual(request.input.text, "Do this")
        XCTAssertEqual(request.lang, "es")
        XCTAssertNil(NotificationAction(item: item, action: "A1", text: "  ", lang: "en"))
        XCTAssertNil(NotificationAction(item: item, action: "A0", text: "unexpected", lang: "en"))
        XCTAssertNil(NotificationAction(item: item, action: "A4", text: nil, lang: "en"))
    }

    private func item() throws -> NotificationPayload.Item {
        let json = #"{"alondra":{"protocol":1,"kind":"question","card":"i-question","data_epoch":"epoch","actions":[{"id":"ans:i-question@117","option":"cookies","label":"Keep cookies","text":false},{"id":"ans:i-question@117","option":"other","label":"Other","text":true,"placeholder":"Tell it what to do"},{"id":"ctx:i-question@117","label":"Need context","text":true,"placeholder":"What is missing"}]}}"#
        let userInfo = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [AnyHashable: Any])
        return try XCTUnwrap(NotificationPayload.Item(userInfo: userInfo))
    }
}
