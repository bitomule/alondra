import AlondraTransport
import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class FeedTests: XCTestCase {
    func testFocusedHomeUsesHomeRouteAndFocusQuery() {
        let route = ViewRoute.parse("home?focus=i-42")
        XCTAssertEqual(route?.path, "home")
        XCTAssertEqual(route?.query.first?.0, "focus")
        XCTAssertEqual(route?.query.first?.1, "i-42")
        XCTAssertTrue(FeedSession.validViewKey("home?focus=i-42"))
        XCTAssertEqual(ViewRoute.parse("card:i-42")?.path, "card:i-42")
        XCTAssertEqual(ViewRoute.parse("pull_request:github.com/bitomule/alondra#154")?.path,
                       "pull_request:github.com/bitomule/alondra#154")
        XCTAssertEqual(ViewRoute.parse("node:a98d034b-249e-4487-a89b-57de38c26372")?.path,
                       "node:a98d034b-249e-4487-a89b-57de38c26372")
        XCTAssertNil(ViewRoute.parse("card:github.com/../alondra#154"))
        XCTAssertNil(ViewRoute.parse("home?focus=../devices"))
        XCTAssertNil(ViewRoute.parse("home?focus="))
    }

    func testViewIDsAreTheOnesTheDaemonAccepts() {
        for id in [
            "hf-weekly-2026-09-30",
            "a98d034b-249e-4487-a89b-57de38c26372",
            "beta-0.3.0-42-result",
            "github.com/bitomule/alondra#154",
            "gitlab.example.com/bitomule/alondra#154",
            "github.example-corp.com/team/repo.name#9007199254740991",
            "gitlab/bitomule/alondra#154",
            "gitlab./bitomule/alondra#154",
        ] {
            XCTAssertTrue(APIRequest.validViewID(id), id)
            XCTAssertEqual(ViewRoute.parse("pull_request:\(id)")?.path, "pull_request:\(id)", id)
            XCTAssertEqual(ViewRoute.parse("home?focus=\(id.addingPercentEncoding(withAllowedCharacters: .alphanumerics)!)")?.query.first?.1, id, id)
        }
        for id in [
            "",
            ".",
            "..",
            "../secret",
            "github.com/../repo#154",
            "github.com/a/b#1/other",
            "github.com/a/b#0",
            "github.com/a/b#1?x=2",
            "github.com/a/B#1",
            "github.com/a/b#9007199254740992",
            "GitLab.example.com/a/b#1",
            "gitlab.example.com/a/b#1/extra",
            "gitlab.example.com/a/b#1#2",
            "github.com/a#1",
            "/a/b#1",
            "bitomule/alondra#688",
            "%2F",
            "a:b",
            "a@b",
            "caf\u{E9}",
            String(repeating: "a", count: 257),
        ] {
            XCTAssertFalse(APIRequest.validViewID(id), id)
            XCTAssertNil(ViewRoute.parse("pull_request:\(id)"), id)
        }
    }

    func testActiveTasksPageUsesTheServerRoute() {
        let key = "tasks?section=active&after=42"
        let route = ViewRoute.parse(key)
        XCTAssertEqual(route?.path, "tasks")
        XCTAssertEqual(route?.query.map { $0.0 }, ["section", "after"])
        XCTAssertEqual(route?.query.map { $0.1 }, ["active", "42"])
        XCTAssertTrue(FeedSession.validViewKey(key))
        XCTAssertNil(ViewRoute.parse("tasks?section=active&after=-1"))
        XCTAssertNil(ViewRoute.parse("tasks?section=unknown&after=42"))
        XCTAssertNil(ViewRoute.parse("tasks?section=archived&after=42"), "cleared work has no page")
        XCTAssertFalse(FeedSession.validViewKey("tasks?section=archived&after=42"))
    }

    func testCanonicalPullRequestDiffSegmentDecodesOnce() {
        let rawID = "github.com/bitomule/alondra#154"
        let fetch = "/pull-requests/github.com%2Fbitomule%2Falondra%23154/diff"
        XCTAssertEqual(AlondraAPIClient.decodedPullRequestID(from: fetch), rawID)
        XCTAssertEqual(AlondraAPIClient.decodedPullRequestID(from: "/pull-requests/pr-154/diff"), "pr-154")
        XCTAssertNil(AlondraAPIClient.decodedPullRequestID(from: "/pull-requests/github.com%252Fbitomule%252Falondra%2523154/diff"))
        XCTAssertNil(AlondraAPIClient.decodedPullRequestID(from: "/pull-requests/github.com%2fbitomule%2falondra%23154/diff"))
        XCTAssertNil(AlondraAPIClient.decodedPullRequestID(from: "/pull-requests/..%2F154/diff"))
        XCTAssertNil(AlondraAPIClient.decodedPullRequestID(from: "/pull-requests/github.com%2Fbitomule%2Falondra%23154/extra/diff"))
    }

    func testConditionalViewResponseIsNotDecodedAsDocument() throws {
        let bytes = Data(#"{"protocol":1,"unchanged":true,"cursor":"c1.next"}"#.utf8)
        let result = try ContractCodec.decode(ViewFetch.self, from: bytes)
        XCTAssertEqual(result, .unchanged(cursor: "c1.next"))
    }

    func testConditionalResponseRejectsOtherProtocol() {
        let bytes = Data(#"{"protocol":2,"unchanged":true,"cursor":"c1.next"}"#.utf8)
        XCTAssertThrowsError(try ContractCodec.decode(ViewFetch.self, from: bytes))
    }

    func testMissingOpenViewKeepsOtherViewsUpdating() async throws {
        let transport = MissingViewTransport()
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        let events = await session.events()
        let watcher = Task { for await _ in events { } }
        defer { watcher.cancel(); Task { await session.stop() } }

        let home = try await session.open("home")
        XCTAssertEqual(home.document?.version, "v1")
        let missing = try await session.open("tasks")
        XCTAssertNil(missing.document)
        let openKeys = await session.openViewKeys
        XCTAssertEqual(openKeys, ["home", "tasks"])

        var updated = false
        for _ in 0..<100 {
            try await Task.sleep(for: .milliseconds(20))
            let snapshot = await session.currentSnapshot()
            let polls = await transport.polls
            if snapshot.documents["home"]?.version == "v2", polls >= 2 {
                updated = true
                XCTAssertNil(snapshot.documents["tasks"])
                break
            }
        }
        XCTAssertTrue(updated, "a missing view must not end the feed for another open view")
    }

    func testIdleLongPollDoesNotPublishRepeatedSnapshots() async throws {
        let transport = IdleViewTransport()
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        let counter = SnapshotCounter()
        let stream = await session.snapshots()
        let watcher = Task {
            for await _ in stream { await counter.add() }
        }
        defer { watcher.cancel(); Task { await session.stop() } }
        _ = try await session.open("home")
        for _ in 0..<100 {
            if await transport.polls >= 2 { break }
            try await Task.sleep(for: .milliseconds(20))
        }
        try await Task.sleep(for: .milliseconds(20))
        let baseline = await counter.value
        for _ in 0..<100 {
            if await transport.polls >= 5 { break }
            try await Task.sleep(for: .milliseconds(20))
        }
        let polls = await transport.polls
        let finalCount = await counter.value
        XCTAssertGreaterThanOrEqual(polls, 5)
        XCTAssertEqual(finalCount, baseline)
    }
}

extension FeedTests {
    /// After a failed poll the Mac is asked to answer at once, so a Mac that is back is known to be
    /// back without waiting out a 50-second long poll; then polls hold again.
    func testAPollAfterAFailureAsksForAnAnswerAtOnce() async throws {
        let transport = FlakyPollTransport()
        let session = FeedSession(transport: transport, client: .ios, lang: "en", retryDelay: { _ in })
        let stream = await session.snapshots()
        let watcher = Task { for await _ in stream {} }
        defer { watcher.cancel(); Task { await session.stop() } }
        _ = try await session.open("home")
        for _ in 0..<100 where await transport.waits.count < 3 { try await Task.sleep(for: .milliseconds(20)) }
        let waits = await transport.waits
        XCTAssertEqual(Array(waits.prefix(3)), [50, 0, 50])
    }
}

private actor FlakyPollTransport: ViewTransport {
    private(set) var waits: [UInt] = []

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        let json = #"{"protocol":1,"view":"home","key":"home","client":"ios","version":"v1","cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}"#
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        waits.append(wait)
        if waits.count == 1 { throw URLError(.networkConnectionLost) }
        if waits.count > 2 { try await Task.sleep(for: .seconds(60)) }
        let json = #"{"protocol":1,"cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"stale":[],"reset":false,"superseded":false}"#
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw FeedError.unknownCommand
    }
}

private actor SnapshotCounter {
    private(set) var value = 0
    func add() { value += 1 }
}

private actor IdleViewTransport: ViewTransport {
    private(set) var polls = 0

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        let json = #"{"protocol":1,"view":"home","key":"home","client":"ios","version":"v1","cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}"#
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        polls += 1
        try await Task.sleep(for: .milliseconds(20))
        let json = #"{"protocol":1,"cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"stale":[],"reset":false,"superseded":false}"#
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw FeedError.unknownCommand
    }
}

private actor MissingViewTransport: ViewTransport {
    private(set) var polls = 0
    private var homeLoads = 0

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        if key == "tasks" { throw FeedError.missingView }
        homeLoads += 1
        let version = homeLoads == 1 ? "v1" : "v2"
        let json = """
        {"protocol":1,"view":"home","key":"home","client":"ios","version":"\(version)",
        "cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready",
        "badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},
        "sections":[]}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        polls += 1
        try await Task.sleep(for: .milliseconds(20))
        let json = """
        {"protocol":1,"cursor":"c\(polls)","data_epoch":"epoch","status":{"version":"s1",
        "summary":"Ready","badge":0,"counts":[],"gauges":[],
        "orchestrator":{"text":"idle","tone":"neutral"}},
        "stale":["home"],"reset":false,"superseded":false}
        """
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw FeedError.unknownCommand
    }
}
