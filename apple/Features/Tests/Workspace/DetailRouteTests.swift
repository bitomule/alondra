import AlondraContract
import AlondraFeatures
import XCTest

final class DetailRouteTests: XCTestCase {
    func testEveryServerLinkInTheFixturesOpensAScreen() throws {
        let keys = try Fixtures.linkedKeys()
        XCTAssertFalse(keys.isEmpty, "the walker found no links")
        let unrouted = keys.filter { DetailRoute.State(key: ViewKey(rawValue: $0), owner: "test") == nil }
        XCTAssertEqual(unrouted, [], "a link the app cannot open")
    }

    func testEachKindOfKeyHasItsScreen() {
        let expected: [(String, DetailRoute.Kind)] = [
            ("card:i-1", .card), ("card:github.com/o/r#1", .card), ("document:rev-1", .document),
            ("pull_request:github.com/o/r#1", .pullRequest), ("task:t-1", .task), ("request:r-1", .request),
            ("node:a-1", .node), ("node:orchestrator", .node), ("schedules", .schedules),
            ("schedule:new", .schedule), ("schedule:s-1", .schedule), ("tasks?section=finished&after=20", .tasksPage),
        ]
        for (key, kind) in expected {
            XCTAssertEqual(DetailRoute.Kind(key: ViewKey(rawValue: key)), kind, key)
        }
    }

    func testAKeyOutsideTheTableOpensNothing() {
        for key in ["", "home", "tasks", "fleet", "new_request", "cards:i-1", "taskz:1", "scheduled", "unknown:1"] {
            XCTAssertNil(DetailRoute.State(key: ViewKey(rawValue: key), owner: "test"), key)
        }
    }

    func testOnlyAViewLinkOpensAScreen() {
        XCTAssertEqual(DetailRoute.State(link: .view(key: "task:t-1"), owner: "o")?.key, ViewKey(rawValue: "task:t-1"))
        XCTAssertNil(DetailRoute.State(link: .url(url: "https://github.com/o/r/pull/1"), owner: "o"))
        XCTAssertNil(DetailRoute.State(link: .local(local: .attach(session: "s")), owner: "o"))
    }
}
