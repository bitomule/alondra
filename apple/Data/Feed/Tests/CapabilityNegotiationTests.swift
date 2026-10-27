import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class CapabilityNegotiationTests: XCTestCase {
    func testModernServiceIsProbedWithoutFlagsThenRefetchedWithoutALegacyKnownVersion() async throws {
        let peer = CapabilityPeer(modern: true)
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        let result = try await session.load("accounts", known: "legacy")
        guard case let .document(document) = result else { return XCTFail("expected document") }
        let requests = await peer.requests
        XCTAssertEqual(requests.map(\.capabilities), [[], ["provider_accounts"]])
        XCTAssertEqual(requests.map(\.known), ["legacy", nil])
        XCTAssertEqual(document.version, "modern")
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.documents["accounts"]?.version, "modern")
        XCTAssertEqual(snapshot.cursor, "modern-cursor")
        await session.stop()
    }

    func testLegacyServiceNeverReceivesAnUnknownQueryParameter() async throws {
        let peer = CapabilityPeer(modern: false)
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("accounts")
        _ = try await session.load("accounts", known: "legacy")
        let requests = await peer.requests
        XCTAssertEqual(requests.map(\.capabilities), [[], []])
        XCTAssertEqual(requests.map(\.known), [nil, "legacy"])
        await session.stop()
    }

    func testFeedAndReconnectUseTheNegotiatedSetAndNeverReuseTheOldCursorOnRenegotiation() async throws {
        let peer = CapabilityPeer(modern: true)
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        let opened = try await session.open("accounts")
        var polls = await peer.polls.makeAsyncIterator()
        var snapshots = await session.snapshots().makeAsyncIterator()
        _ = await snapshots.next()
        let poll = await polls.next()
        withExtendedLifetime(snapshots) {}
        XCTAssertEqual(poll?.capabilities, ["provider_accounts"])
        XCTAssertEqual(poll?.cursor, "modern-cursor")
        await peer.setModern(false)
        await session.reconnect()
        _ = try await session.load("accounts")
        let requests = await peer.requests
        XCTAssertEqual(requests.last?.capabilities, [])
        XCTAssertNil(requests.last?.known)
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.documents["accounts"]?.version, "legacy")
        XCTAssertEqual(snapshot.cursor, "legacy-cursor")
        await session.close(opened.lease)
        await session.stop()
    }
}

private struct CapabilityRequest: Sendable {
    let capabilities: [String]
    let known: String?
}
private struct CapabilityPoll: Sendable {
    let capabilities: [String]
    let cursor: String?
}
private actor CapabilityPeer: ViewTransport {
    var modern: Bool
    private(set) var requests: [CapabilityRequest] = []
    let polls: AsyncStream<CapabilityPoll>
    private let pollContinuation: AsyncStream<CapabilityPoll>.Continuation
    init(modern: Bool) {
        self.modern = modern
        (polls, pollContinuation) = AsyncStream.makeStream(bufferingPolicy: .unbounded)
    }
    func setModern(_ value: Bool) { modern = value }
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        try await fetchView(key: key, client: client, lang: lang, known: known, capabilities: [])
    }
    func fetchView(key: String, client: ViewClient, lang: String, known: String?, capabilities: [String]) async throws -> ViewFetch {
        requests.append(CapabilityRequest(capabilities: capabilities, known: known))
        let version = modern && !capabilities.isEmpty ? "modern" : "legacy"
        let service = modern ? #", "service":{"version":"test","views_protocol":{"min":1,"max":1},"capabilities":["provider_accounts","unimplemented_feature"]}"# : ""
        let json = """
        {"protocol":1,"view":"accounts","key":"accounts","client":"ios","version":"\(version)",
        "cursor":"\(version)-cursor","data_epoch":"epoch","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]\(service)}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        try await fetchChanges(client: client, lang: lang, wait: wait, cursor: cursor, views: views, capabilities: [])
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion], capabilities: [String]) async throws -> ViewFeed {
        pollContinuation.yield(CapabilityPoll(capabilities: capabilities, cursor: cursor))
        throw CancellationError()
    }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse { throw CancellationError() }
}
