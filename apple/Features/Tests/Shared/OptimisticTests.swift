import AlondraContract
import AlondraFeatures
import XCTest

final class OptimisticTests: XCTestCase {
    private func request(_ command: String) -> ActionRequest {
        ActionRequest(commandId: command, dataEpoch: "", client: .ios, lang: "en", actionId: "accept:x@1")
    }

    func testAnItemIsHiddenFromTheMomentItIsSentUntilANewerViewArrives() {
        var optimistic = Optimistic()
        optimistic.begin("card", request("a"))
        XCTAssertTrue(optimistic.hides("card"))

        optimistic.reconcile("v1")
        XCTAssertTrue(optimistic.hides("card"), "a view never settles what is still being sent")

        XCTAssertNil(optimistic.settle(request("a"), applied: true, shown: "v1"))
        optimistic.reconcile("v1")
        XCTAssertTrue(optimistic.hides("card"), "the view the daemon applied it over is stale")

        optimistic.reconcile("v2")
        XCTAssertFalse(optimistic.hides("card"))
        XCTAssertTrue(optimistic.isEmpty)
    }

    func testAnActionTheDaemonDidNotApplyGivesItsItemBack() {
        var optimistic = Optimistic()
        optimistic.begin("card", request("a"))

        XCTAssertEqual(optimistic.settle(request("a"), applied: false, shown: "v1"), "card")
        XCTAssertFalse(optimistic.hides("card"))
        XCTAssertNil(optimistic.settle(request("a"), applied: false, shown: "v1"), "an answer is settled once")
    }

    func testAnItemStillBeingSentAgainStaysHiddenWhenAnEarlierSendFails() {
        var optimistic = Optimistic()
        optimistic.begin("card", request("a"))
        optimistic.begin("card", request("b"))

        XCTAssertNil(optimistic.settle(request("a"), applied: false, shown: nil))
        XCTAssertTrue(optimistic.hides("card"))
    }
}
