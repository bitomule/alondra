import AlondraViews
import AlondraViewsDependency
import XCTest

final class ViewsClientTests: XCTestCase {
    func testViewKeyIsAStableSingleValue() throws {
        let key = ViewKey.card("interaction-42")
        let bytes = try JSONEncoder().encode(key)
        XCTAssertEqual(String(decoding: bytes, as: UTF8.self), "\"card:interaction-42\"")
        XCTAssertEqual(try JSONDecoder().decode(ViewKey.self, from: bytes), key)
    }
}
