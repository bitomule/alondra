import AlondraNotifications
import UserNotifications
import XCTest

final class ForegroundPresentationTests: XCTestCase {
    func testRemotePushIsSilentWhileAppIsOpen() {
        XCTAssertEqual(ForegroundPresentation.options(isRemotePush: true), [])
    }

    func testLocalNotificationStillShowsWhileAppIsOpen() {
        XCTAssertEqual(ForegroundPresentation.options(isRemotePush: false), [.banner, .list, .badge])
    }
}
