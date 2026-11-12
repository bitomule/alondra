import AlondraiOSUI
import XCTest

/// A time before now reads as past and one after it as future, in both languages.
@MainActor
final class RelativeTimeTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_790_762_400)
    private let english = Locale(identifier: "en")
    private let spanish = Locale(identifier: "es")

    func testAPastTimeReadsAsPast() {
        let past = now.addingTimeInterval(-8 * 60)
        let inEnglish = RelativeTime.phrase(for: past, relativeTo: now, locale: english)
        let inSpanish = RelativeTime.phrase(for: past, relativeTo: now, locale: spanish)
        XCTAssertTrue(inEnglish.hasSuffix("ago"), inEnglish)
        XCTAssertTrue(inSpanish.hasPrefix("hace"), inSpanish)
    }

    func testAFutureTimeReadsAsFuture() {
        let future = now.addingTimeInterval(8 * 60)
        let inEnglish = RelativeTime.phrase(for: future, relativeTo: now, locale: english)
        let inSpanish = RelativeTime.phrase(for: future, relativeTo: now, locale: spanish)
        XCTAssertTrue(inEnglish.hasPrefix("in "), inEnglish)
        XCTAssertTrue(inSpanish.hasPrefix("dentro de"), inSpanish)
    }
}
