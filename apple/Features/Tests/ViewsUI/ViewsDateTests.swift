import AlondraContract
import AlondraDesign
import AlondraFeatures
import XCTest

/// The Mac's relative times: before now they read as past, after it as future, in both languages.
final class ViewsDateTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_790_762_400)

    override func setUp() {
        RelativeClock.now = { [now] in now }
    }

    override func tearDown() {
        RelativeClock.now = { Date() }
    }

    private func iso(_ offset: TimeInterval) -> String {
        ISO8601DateFormatter().string(from: now.addingTimeInterval(offset))
    }

    func testAPastTimeReadsAsPast() {
        let past = iso(-8 * 60)
        let inEnglish = ViewsDate.display(past, locale: Locale(identifier: "en"))
        let inSpanish = ViewsDate.display(past, locale: Locale(identifier: "es"))
        XCTAssertTrue(inEnglish.hasSuffix("ago"), inEnglish)
        XCTAssertTrue(inSpanish.hasPrefix("hace"), inSpanish)
    }

    func testAFutureTimeReadsAsFuture() {
        let future = iso(8 * 60)
        let inEnglish = ViewsDate.display(future, locale: Locale(identifier: "en"))
        let inSpanish = ViewsDate.display(future, locale: Locale(identifier: "es"))
        XCTAssertTrue(inEnglish.hasPrefix("in "), inEnglish)
        XCTAssertTrue(inSpanish.hasPrefix("dentro de"), inSpanish)
    }
}
