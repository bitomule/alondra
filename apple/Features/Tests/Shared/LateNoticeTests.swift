import AlondraContract
import AlondraFeatures
import XCTest

final class LateNoticeTests: XCTestCase {
    func testAnUnreachableMacSaysWhyFirstAndThatItKeepsSending() {
        XCTAssertEqual(LateNotice.action("Cancel Task").message(.unreachable(.noInternet)),
                       "No internet: still sending “Cancel Task”.")
        XCTAssertEqual(LateNotice.answer("Merge the PR?").message(.unreachable(.macNotAnswering)),
                       "Your Mac isn’t answering: still sending your answer to “Merge the PR?”.")
    }

    func testAConnectionStillStartingSaysSo() {
        XCTAssertEqual(LateNotice.action("Cancel Task").message(.connecting),
                       "Connecting to your Mac: still sending “Cancel Task”.")
        XCTAssertEqual(LateNotice.answer("Merge the PR?").message(.connecting),
                       "Connecting to your Mac: still sending your answer to “Merge the PR?”.")
    }

    func testAConnectedMacThatIsSlowOnlySaysItHasNotConfirmedYet() {
        XCTAssertEqual(LateNotice.action("Cancel Task").message(.connected(.direct)),
                       "Your Mac hasn’t confirmed it yet: still sending “Cancel Task”.")
        XCTAssertEqual(LateNotice.answer("Merge the PR?").message(nil),
                       "Your Mac hasn’t confirmed it yet: still sending your answer to “Merge the PR?”.")
    }

    func testWithoutANameItStillSaysWhatKindOfThingIsLate() {
        XCTAssertEqual(LateNotice.action(nil).message(.unreachable(.noInternet)), "Your Mac hasn’t confirmed an action.")
        XCTAssertEqual(LateNotice.answer("").message(nil), "Your Mac hasn’t confirmed an answer.")
    }
}
