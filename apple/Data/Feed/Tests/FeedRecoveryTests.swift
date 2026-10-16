import AlondraContract
@testable import AlondraFeed
import AlondraTransport
import Foundation
import XCTest

final class FeedRecoveryTests: XCTestCase {
    private func version(_ stream: inout AsyncThrowingStream<ViewFetch, any Error>.Iterator) async throws -> String? {
        if case .document(let document) = try await stream.next() { document.version } else { nil }
    }

    /// Ends the feed if the test would otherwise wait forever, so a broken recovery fails instead of hanging.
    private func watchdog(_ session: FeedSession) -> Task<Void, any Error> {
        Task {
            try await Task.sleep(for: .seconds(2))
            await session.stop()
        }
    }

    func testTheFeedSurvivesAMacRestartAndReloadsTheNewData() async throws {
        let transport = RestartingMac(firstPoll: APIError.conflict(kind: nil, detail: "uncertain", reasons: []))
        let delays = Delays()
        let session = FeedSession(transport: transport, client: .ios, lang: "en", retryDelay: { await delays.record($0) })
        let stopper = watchdog(session)
        defer { stopper.cancel() }
        var fleet = ViewObservation.stream(session: session, key: .fleet).makeAsyncIterator()

        let before = try await version(&fleet)
        XCTAssertEqual(before, "v-old")
        let after = try await version(&fleet)

        XCTAssertEqual(after, "v-new", "the poll after the failure is answered with a reset and the view reloads")
        let attempts = await delays.attempts
        XCTAssertEqual(attempts, [1])
        let cursors = await transport.cursors
        XCTAssertEqual(Array(cursors.prefix(2)), ["c-old", "c-old"], "the retry resumes from the same cursor")
        await session.stop()
    }

    func testARefusedDeviceEndsTheFeedWithoutRetrying() async throws {
        let transport = RestartingMac(firstPoll: APIError.unauthorized)
        let delays = Delays()
        let session = FeedSession(transport: transport, client: .ios, lang: "en", retryDelay: { await delays.record($0) })
        let stopper = watchdog(session)
        defer { stopper.cancel() }
        var fleet = ViewObservation.stream(session: session, key: .fleet).makeAsyncIterator()

        _ = try await version(&fleet)
        try await Task.sleep(for: .milliseconds(200))

        let attempts = await delays.attempts
        XCTAssertEqual(attempts, [])
        let polls = await transport.cursors.count
        XCTAssertEqual(polls, 1)
        await session.stop()
    }

    func testOnlyErrorsARetryCanFixAreTransient() {
        XCTAssertTrue(FeedSession.isTransient(URLError(.networkConnectionLost)))
        XCTAssertTrue(FeedSession.isTransient(OperationError.outcomeUnknown))
        XCTAssertTrue(FeedSession.isTransient(APIError.conflict(kind: nil, detail: "uncertain", reasons: [])))
        XCTAssertTrue(FeedSession.isTransient(APIError.server(status: 429, kind: nil, detail: "changes_busy", reasons: [])))
        XCTAssertTrue(FeedSession.isTransient(APIError.server(status: 502, kind: nil, detail: "read_failed", reasons: [])))
        XCTAssertTrue(FeedSession.isTransient(APIError.unavailable))
        XCTAssertFalse(FeedSession.isTransient(APIError.unauthorized))
        XCTAssertFalse(FeedSession.isTransient(APIError.forbidden(kind: nil, detail: nil, reasons: [])))
        XCTAssertFalse(FeedSession.isTransient(RelayWireError.unauthorized))
        XCTAssertFalse(FeedSession.isTransient(APIError.server(status: 400, kind: nil, detail: "invalid_changes_query", reasons: [])))
        XCTAssertFalse(FeedSession.isTransient(DirectSessionError.deviceRevoked))
    }

    /// The changes loop ended for good on these, so the iPhone never heard of a change again until
    /// it was killed: a relay that could not reach the Mac, a Mac with no direct route.
    func testLosingEveryRouteToTheMacIsRetried() {
        XCTAssertTrue(FeedSession.isTransient(SealedRequestError.outcomeUnknown))
        XCTAssertTrue(FeedSession.isTransient(DirectRouteError.unreachable))
        XCTAssertTrue(FeedSession.isTransient(DirectRouteError.localNetworkDenied))
        XCTAssertTrue(FeedSession.isTransient(URLError(.notConnectedToInternet)))
    }

    func testReconnectingTellsEveryObserver() async {
        let session = FeedSession(transport: FailingTransport(), client: .ios, lang: "en")
        var first = await session.reconnections().makeAsyncIterator()
        var second = await session.reconnections().makeAsyncIterator()
        await session.reconnect()
        let heard = (await first.next() != nil, await second.next() != nil)
        XCTAssertTrue(heard.0 && heard.1)
        await session.stop()
        let afterStop = await first.next()
        XCTAssertNil(afterStop, "a stopped session ends its reconnections")
    }
}

private actor Delays {
    private(set) var attempts: [Int] = []
    func record(_ attempt: Int) { attempts.append(attempt) }
}

/// A Mac that restarts during the first long poll: that poll fails, and the store it comes back
/// with has a new data epoch and a new version of every view.
private actor RestartingMac: ViewTransport {
    private let firstPoll: any Error
    private var restarted = false
    private(set) var cursors: [String?] = []

    init(firstPoll: any Error) { self.firstPoll = firstPoll }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        let (version, epoch, cursor) = restarted ? ("v-new", "e-new", "c-new") : ("v-old", "e-old", "c-old")
        let json = """
        {"protocol":1,"view":"\(key)","key":"\(key)","client":"ios","version":"\(version)",
        "cursor":"\(cursor)","data_epoch":"\(epoch)","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        cursors.append(cursor)
        if cursors.count == 1 {
            restarted = true
            throw firstPoll
        }
        if cursors.count > 2 {
            try await Task.sleep(for: .seconds(60))
        }
        let json = """
        {"protocol":1,"cursor":"c-new","data_epoch":"e-new","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},
        "stale":[],"reset":\(cursor == "c-new" ? "false" : "true"),"superseded":false}
        """
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw FeedError.unknownCommand
    }
}

final class FeedSupersededTests: XCTestCase {
    /// Over the relay a poll this session already gave up on can reach the Mac after the live one,
    /// which the Mac then answers "superseded". The feed used to end there, so nothing the Mac
    /// changed reached the phone again: an answered card stayed on screen.
    func testAPollTheMacSupersededIsAskedAgainAndTheChangeArrives() async throws {
        let transport = SupersedingMac()
        let delays = Delays()
        let session = FeedSession(transport: transport, client: .ios, lang: "en", retryDelay: { await delays.record($0) })
        let stopper = Task {
            try await Task.sleep(for: .seconds(5))
            await session.stop()
        }
        defer { stopper.cancel() }
        var home = ViewObservation.stream(session: session, key: .home).makeAsyncIterator()

        guard case .document(let before)? = try await home.next() else { return XCTFail("home never loaded") }
        XCTAssertEqual(before.version, "v1")
        guard case .document(let after)? = try await home.next() else { return XCTFail("the change after a superseded poll never arrived") }
        XCTAssertEqual(after.version, "v2")
        let attempts = await delays.attempts
        XCTAssertEqual(attempts, [1], "it waits before asking again, as after any failed poll")
        await session.stop()
    }
}

private actor SupersedingMac: ViewTransport {
    private var polls = 0
    private var version = "v1"

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        let json = """
        {"protocol":1,"view":"\(key)","key":"\(key)","client":"ios","version":"\(version)",
        "cursor":"c-\(version)","data_epoch":"e","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        polls += 1
        if polls > 2 { try await Task.sleep(for: .seconds(60)) }
        if polls == 2 { version = "v2" }
        let json = """
        {"protocol":1,"cursor":"c-\(version)","data_epoch":"e","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},
        "stale":\(polls == 2 ? "[\"home\"]" : "[]"),"reset":false,"superseded":\(polls == 1)}
        """
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw FeedError.unknownCommand
    }
}

private struct FailingTransport: ViewTransport {
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        throw URLError(.notConnectedToInternet)
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw URLError(.notConnectedToInternet)
    }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw URLError(.notConnectedToInternet)
    }
}
