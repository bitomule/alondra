import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class ViewOwnerRetentionTests: XCTestCase {
    func testClosedOwnersDoNotAccumulateBookkeeping() async throws {
        for sample in 0..<3 {
            let peer = OwnerViewPeer()
            let session = FeedSession(transport: peer, client: .ios, lang: "en")
            let opened = ViewOpenSet(session: session)
            var completed = 0
            var peakGenerations = 0
            for index in 0..<1000 {
                let owner = "screen-\(index)"
                await opened.replace(owner: owner, with: [.schedules])
                let observation = await Self.observe(opened, owner: owner)
                if observation.hasLease { completed += 1 }
                peakGenerations = max(peakGenerations, observation.generations)
                await opened.remove(owner: owner)
            }
            let generations = await opened.generations
            let owners = await opened.owners.count
            let keys = await session.openViewKeys
            let snapshot = await session.currentSnapshot()
            let calls = await peer.calls
            let returned = await peer.returnedBytes
            let keyBytes = generations.keys.reduce(0) { $0 + $1.utf8.count }
            await session.stop()
            print("owner_retention sample=\(sample) offered=1000 completed=\(completed) errors=\(1000 - completed) fetches=\(calls) peak_generations=\(peakGenerations) returned_bytes=\(returned) retained_generations=\(generations.count) retained_key_bytes=\(keyBytes) owners=\(owners) open_keys=\(keys.count) documents=\(snapshot.documents.count)")
            XCTAssertEqual(completed, 1000, "every offered opening must acquire its actual lease before close")
            XCTAssertEqual(calls, 1000)
            XCTAssertEqual(owners, 0)
            XCTAssertTrue(keys.isEmpty)
            XCTAssertTrue(snapshot.documents.isEmpty)
            XCTAssertTrue(generations.isEmpty, "closed screen identities must not accumulate in the authenticated session")
            XCTAssertEqual(keyBytes, 0)
        }
    }

    private static func observe(_ opened: isolated ViewOpenSet, owner: String) -> (hasLease: Bool, generations: Int) {
        (opened.owners[owner]?.count == 1, opened.generations.count)
    }

    func testCompletedEmptyReplacementsDoNotRetainOwnerIdentities() async throws {
        let peer = OwnerViewPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        let opened = ViewOpenSet(session: session)
        for index in 0..<1000 { await opened.replace(owner: "empty-\(index)", with: []) }
        let owners = await opened.owners.count
        let generations = await opened.generations.count
        let calls = await peer.calls
        await session.stop()
        print("empty_owner_retention offered=1000 completed=1000 errors=0 fetches=\(calls) owners=\(owners) generations=\(generations)")
        XCTAssertEqual(calls, 0)
        XCTAssertEqual(owners, 0, "a completed empty replacement has no leases or pending opening to retain")
        XCTAssertEqual(generations, 0)
    }

    func testLateOpeningCannotRejoinAClosedAndReopenedOwnerOrCloseItsPeer() async throws {
        let peer = OwnerViewPeer(holdSchedules: true)
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        let opened = ViewOpenSet(session: session)
        _ = try await session.load("home")
        var starts = await peer.starts().makeAsyncIterator()
        let old = Task { await opened.replace(owner: "same", with: [.schedules]) }
        let entered = await starts.next()
        XCTAssertNotNil(entered)
        await opened.remove(owner: "same")
        await opened.replace(owner: "same", with: [.tasks])
        await opened.replace(owner: "peer", with: [.tasks])
        let pendingKeys = await session.openViewKeys
        XCTAssertEqual(pendingKeys, ["schedules", "tasks"], "the original opening must remain live until its peer replies")
        await peer.release()
        await old.value
        let owners = await opened.owners
        let keys = await session.openViewKeys
        let calls = await peer.calls
        XCTAssertEqual(owners["same"]?.keys.map(\.rawValue).sorted(), ["tasks"])
        XCTAssertEqual(owners["peer"]?.keys.map(\.rawValue).sorted(), ["tasks"])
        XCTAssertEqual(keys, ["tasks"], "the old opening must release its schedules lease after the owner is reopened")
        let requested = await peer.keys
        XCTAssertEqual(calls, 3, "home seed, held schedules and one tasks load for both owners")
        XCTAssertEqual(requested, ["home", "schedules", "tasks"], "the old opening must not be replaced by epoch recovery")
        await opened.remove(owner: "same")
        let peerKeys = await session.openViewKeys
        XCTAssertEqual(peerKeys, ["tasks"], "closing one consumer must preserve its peer's lease")
        await opened.remove(owner: "peer")
        let closedKeys = await session.openViewKeys
        XCTAssertTrue(closedKeys.isEmpty)
        await session.stop()
    }
}

private actor OwnerViewPeer: ViewTransport {
    private var holdSchedules: Bool
    private var pending: (ViewFetch, CheckedContinuation<ViewFetch, any Error>)?
    private let stream: AsyncStream<Void>
    private let entered: AsyncStream<Void>.Continuation
    private(set) var calls = 0
    private(set) var returnedBytes = 0
    private(set) var keys: [String] = []

    init(holdSchedules: Bool = false) {
        self.holdSchedules = holdSchedules
        let pair = AsyncStream<Void>.makeStream()
        stream = pair.stream; entered = pair.continuation
    }

    func starts() -> AsyncStream<Void> { stream }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        calls += 1
        keys.append(key)
        let body: [String: Any] = ["protocol": 1, "view": key, "key": key, "client": client.rawValue,
            "version": "current", "cursor": "current", "data_epoch": "owner-epoch",
            "status": ["version": "s", "summary": "Ready", "badge": 0, "counts": [], "gauges": [],
                "orchestrator": ["text": "idle", "tone": "neutral"]], "sections": []]
        let bytes = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        returnedBytes += bytes.count
        let document = try ContractCodec.decode(ViewFetch.self, from: bytes)
        if key == "schedules", holdSchedules {
            holdSchedules = false
            return try await withCheckedThrowingContinuation { completion in
                pending = (document, completion)
                entered.yield(())
            }
        }
        return document
    }

    func release() {
        let completion = pending
        pending = nil
        if let completion { completion.1.resume(returning: completion.0) }
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw CancellationError()
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw CancellationError()
    }
}
