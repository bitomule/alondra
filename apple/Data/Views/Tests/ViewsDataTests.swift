import AlondraViews
@testable import AlondraViewsData
import Foundation
import XCTest

final class ViewsDataTests: XCTestCase {
    func testFocusedHomeUsesHomeRouteAndFocusQuery() {
        let route = ViewRoute.parse("home?focus=i-42")
        XCTAssertEqual(route?.path, "home")
        XCTAssertEqual(route?.query.first?.0, "focus")
        XCTAssertEqual(route?.query.first?.1, "i-42")
        XCTAssertTrue(ViewsFeedSession.validViewKey("home?focus=i-42"))
        XCTAssertEqual(ViewRoute.parse("card:i-42")?.path, "card:i-42")
        XCTAssertNil(ViewRoute.parse("home?focus=../devices"))
        XCTAssertNil(ViewRoute.parse("home?focus="))
    }

    func testConditionalViewResponseIsNotDecodedAsDocument() throws {
        let bytes = Data(#"{"protocol":1,"unchanged":true,"cursor":"c1.next"}"#.utf8)
        let result = try ViewsCodec.decode(ViewFetch.self, from: bytes)
        XCTAssertEqual(result, .unchanged(cursor: "c1.next"))
    }

    func testConditionalResponseRejectsOtherProtocol() {
        let bytes = Data(#"{"protocol":2,"unchanged":true,"cursor":"c1.next"}"#.utf8)
        XCTAssertThrowsError(try ViewsCodec.decode(ViewFetch.self, from: bytes))
    }
}
