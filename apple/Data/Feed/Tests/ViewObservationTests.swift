import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class ViewObservationTests: XCTestCase {
    private func nextVersion(_ stream: inout AsyncThrowingStream<ViewFetch, any Error>.Iterator) async throws -> String? {
        if case .document(let document) = try await stream.next() { document.version } else { nil }
    }

    private func session(_ transport: ScriptedTransport) -> FeedSession {
        FeedSession(transport: transport, client: .ios, lang: "en")
    }

    func testYieldsTheFirstDocumentThenANewerOneEachTimeTheKeyChanges() async throws {
        let transport = ScriptedTransport(versions: ["tasks": "v1"])
        let session = session(transport)
        var documents = ViewObservation.stream(session: session, key: ViewKey(rawValue: "tasks")).makeAsyncIterator()

        let version = try await nextVersion(&documents)

        XCTAssertEqual(version, "v1")
        await transport.serve("tasks", version: "v2")
        await transport.changed(["tasks"])
        let refreshed = try await nextVersion(&documents)
        XCTAssertEqual(refreshed, "v2")
        await session.stop()
    }

    func testChangesToOtherKeysAndUnchangedRefreshesYieldNothing() async throws {
        let transport = ScriptedTransport(versions: ["tasks": "v1", "fleet": "f1"])
        let session = session(transport)
        _ = try await session.open("fleet")
        var documents = ViewObservation.stream(session: session, key: ViewKey(rawValue: "tasks")).makeAsyncIterator()
        let version = try await nextVersion(&documents)
        XCTAssertEqual(version, "v1")

        await transport.changed(["fleet"])
        await transport.changed(["tasks"])
        await transport.serve("tasks", version: "v3")
        await transport.changed(["tasks"])

        let refreshed = try await nextVersion(&documents)
        XCTAssertEqual(
            refreshed, "v3",
            "the stale for fleet and the stale that found tasks unchanged must not produce a document"
        )
        await session.stop()
    }

    func testAKeyTheDaemonDoesNotServeEndsTheStreamAsMissing() async throws {
        let transport = ScriptedTransport(versions: [:])
        let session = session(transport)
        do {
            for try await _ in ViewObservation.stream(session: session, key: ViewKey(rawValue: "tasks")) {
                XCTFail("nothing to show")
            }
            XCTFail("the stream must throw")
        } catch {
            XCTAssertEqual(error as? ViewsClientError, .missingView)
        }
        await session.stop()
    }

    func testAKeyThatDisappearsLaterEndsTheStreamAsMissing() async throws {
        let transport = ScriptedTransport(versions: ["tasks": "v1"])
        let session = session(transport)
        var documents = ViewObservation.stream(session: session, key: ViewKey(rawValue: "tasks")).makeAsyncIterator()
        let version = try await nextVersion(&documents)
        XCTAssertEqual(version, "v1")

        await transport.serve("tasks", version: nil)
        await transport.changed(["tasks"])
        do {
            _ = try await nextVersion(&documents)
            XCTFail("the stream must throw")
        } catch {
            XCTAssertEqual(error as? ViewsClientError, .missingView)
        }
        await session.stop()
    }

    func testReleasesTheKeyWhenTheConsumerStops() async throws {
        let transport = ScriptedTransport(versions: ["tasks": "v1"])
        let session = session(transport)
        var snapshots = await session.snapshots().makeAsyncIterator()
        do {
            var documents = ViewObservation.stream(session: session, key: ViewKey(rawValue: "tasks")).makeAsyncIterator()
            let version = try await nextVersion(&documents)
            XCTAssertEqual(version, "v1")
        }
        while let snapshot = await snapshots.next() {
            if snapshot.documents["tasks"] == nil { break }
        }
        let open = await session.openViewKeys
        XCTAssertEqual(open, [])
        await session.stop()
    }
}

/// The daemon as a script: what it serves per key, and when it says something changed. Nothing waits
/// on a clock; a long poll returns when the test calls `changed`.
private actor ScriptedTransport: ViewTransport {
    private var versions: [String: String?]
    private let changes = Changes()

    init(versions: [String: String]) {
        self.versions = versions.mapValues { Optional($0) }
    }

    func serve(_ key: String, version: String?) { versions[key] = .some(version) }
    func changed(_ keys: [String]) async { await changes.push(keys) }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        guard let served = versions[key], let version = served else { throw FeedError.missingView }
        if known == version { return .unchanged(cursor: "c") }
        let json = """
        {"protocol":1,"view":"\(key)","key":"\(key)","client":"ios","version":"\(version)",
        "cursor":"c","data_epoch":"epoch","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        guard let stale = await changes.next() else { throw CancellationError() }
        let json = """
        {"protocol":1,"cursor":"c","data_epoch":"epoch","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},
        "stale":\(String(decoding: try JSONEncoder().encode(stale), as: UTF8.self)),"reset":false,"superseded":false}
        """
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw FeedError.unknownCommand
    }
}

/// A queue the long poll waits on; cancelling the wait ends it, so a stopped feed never hangs.
private actor Changes {
    private var pending: [[String]] = []
    private var waiting: CheckedContinuation<[String]?, Never>?

    func push(_ keys: [String]) {
        if let waiting {
            self.waiting = nil
            waiting.resume(returning: keys)
        } else {
            pending.append(keys)
        }
    }

    func next() async -> [String]? {
        if !pending.isEmpty { return pending.removeFirst() }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { waiting = $0 }
        } onCancel: {
            Task { await self.cancelWaiting() }
        }
    }

    private func cancelWaiting() {
        waiting?.resume(returning: nil)
        waiting = nil
    }
}
