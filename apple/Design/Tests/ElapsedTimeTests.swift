import AlondraContract
import AlondraDesign
import Foundation
import XCTest

final class ElapsedTimeTests: XCTestCase {
    func testDurationsReadInMinutesHoursAndDaysAsTheDaemonWritesThem() {
        for locale in [Locale(identifier: "en_US"), Locale(identifier: "es_ES")] {
            XCTAssertEqual(
                [0, 59, 60, 3_599, 3_600, 3_900, 86_399, 86_400, 2 * 86_400 + 3 * 3_600].map { ElapsedTime.text(seconds: $0, locale: locale) },
                ["<1 min", "<1 min", "1 min", "59 min", "1 h 00 min", "1 h 05 min", "23 h 59 min", "1 d 0 h", "2 d 3 h"],
                locale.identifier
            )
        }
    }

    func testAnOpenSpanCountsUpToNowAndAClosedOneStaysAtItsTotal() {
        let locale = Locale(identifier: "en_US")
        let open = AlondraContract.Label(text: "running since 08:05", tone: .neutral, elapsed: Elapsed(since: "2026-09-30T08:05:00.000+02:00"))
        let start = ISO8601DateFormatter().date(from: "2026-09-30T06:05:00Z")!
        XCTAssertEqual(ElapsedTime.text(open, now: start.addingTimeInterval(12 * 60), locale: locale), "12 min")
        XCTAssertEqual(ElapsedTime.text(open, now: start.addingTimeInterval(65 * 60), locale: locale), "1 h 05 min")

        let closed = AlondraContract.Label(text: "took 1 h 05 min", tone: .neutral,
                                           elapsed: Elapsed(since: "2026-09-30T08:05:00.000+02:00", until: "2026-09-30T09:10:00.000+02:00"))
        XCTAssertEqual(ElapsedTime.text(closed, now: start.addingTimeInterval(5 * 86_400), locale: locale), "1 h 05 min")
    }

    func testALabelWithoutASpanOrWithAnUnreadableOneKeepsItsText() {
        let locale = Locale(identifier: "en_US")
        XCTAssertEqual(ElapsedTime.text(AlondraContract.Label(text: "Working", tone: .info), now: .now, locale: locale), "Working")
        let broken = AlondraContract.Label(text: "running since 08:05", tone: .neutral, elapsed: Elapsed(since: "08:05"))
        XCTAssertEqual(ElapsedTime.text(broken, now: .now, locale: locale), "running since 08:05")
    }
}
