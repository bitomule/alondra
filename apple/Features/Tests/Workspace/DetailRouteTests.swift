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

    func testTheRebuiltScreensOpenTheirOwnFeatureAndTheRestStayOnTheLegacyRenderer() throws {
        func route(_ key: String) throws -> DetailRoute.State {
            try XCTUnwrap(DetailRoute.State(key: ViewKey(rawValue: key), owner: "test"), key)
        }

        for key in ["card:i-1", "card:github.com/o/r#1", "document:rev-1"] {
            guard case .review = try route(key) else { return XCTFail("\(key) must open the review") }
        }
        guard case .taskDetail = try route("task:t-1") else { return XCTFail("task") }
        guard case .requestDetail = try route("request:r-1") else { return XCTFail("request") }
        guard case .schedules = try route("schedules") else { return XCTFail("schedules") }
        guard case .scheduleEditor = try route("schedule:new") else { return XCTFail("new schedule") }
        guard case .scheduleEditor = try route("schedule:s-1") else { return XCTFail("schedule") }
        for key in ["pull_request:github.com/o/r#1", "node:a-1", "tasks?section=finished&after=20"] {
            guard case .legacy = try route(key) else { return XCTFail("\(key) is not on the legacy renderer yet") }
        }
    }

    func testTheKeyOfARebuiltScreenIsTheKeyItWasOpenedWith() throws {
        for key in ["card:i-1", "document:rev-1", "task:t-1", "request:r-1", "schedules", "schedule:new", "schedule:s-1"] {
            XCTAssertEqual(DetailRoute.State(key: ViewKey(rawValue: key), owner: "test")?.key.rawValue, key)
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
