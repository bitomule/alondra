import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class TransientViewRetentionTests: XCTestCase {
    func testOneOffPickerResultsReturnWithoutRetainingHistoricalDocuments() async throws {
        let peer = PickerViewPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        var completed = 0
        var errors = 0
        for index in 0..<64 {
            let key = "issue_picker?q=query-\(index)"
            do {
                let result = try await session.load(key)
                if case let .document(document) = result,
                   document.key == key, document.status.summary.utf8.count == 8192 { completed += 1 }
                else { errors += 1 }
            } catch { errors += 1 }
        }
        let snapshot = await session.currentSnapshot()
        let owners = await session.loadOwners.count
        let lifetimes = await session.loadLifetimes.count
        let keys = await session.openViewKeys
        let calls = await peer.calls
        let returned = await peer.returnedBytes
        let retained = snapshot.documents.values.reduce(0) { $0 + $1.status.summary.utf8.count }
        await session.stop()
        print("transient_picker offered=64 completed=\(completed) errors=\(errors) calls=\(calls) returned_bytes=\(returned) documents=\(snapshot.documents.count) retained_summary_bytes=\(retained) owners=\(owners) lifetimes=\(lifetimes) open_keys=\(keys.count)")
        XCTAssertEqual(completed, 64)
        XCTAssertEqual(errors, 0)
        XCTAssertEqual(calls, 64)
        XCTAssertTrue(keys.isEmpty)
        XCTAssertTrue(snapshot.documents.isEmpty, "one-off picker results belong to their caller rather than an unbounded historical cache")
        XCTAssertEqual(retained, 0)
        XCTAssertEqual(owners, 0)
        XCTAssertEqual(lifetimes, 0)
    }

    func testLeasedPickerAndOrdinaryUnleasedViewsKeepTheirExistingCache() async throws {
        let peer = PickerViewPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("home")
        let pinned = try await session.open("issue_picker?q=pinned")
        for index in 0..<16 { _ = try await session.load("issue_picker?q=other-\(index)") }
        let snapshot = await session.currentSnapshot()
        let keys = await session.openViewKeys
        await session.close(pinned.lease)
        let closed = await session.currentSnapshot()
        await session.stop()
        XCTAssertEqual(snapshot.documents["home"]?.key, "home", "ordinary one-off cache semantics are unchanged")
        XCTAssertEqual(snapshot.documents["issue_picker?q=pinned"]?.key, "issue_picker?q=pinned")
        XCTAssertEqual(keys, ["issue_picker?q=pinned"])
        XCTAssertEqual(Set(snapshot.documents.keys), ["home", "issue_picker?q=pinned"])
        XCTAssertNil(closed.documents["issue_picker?q=pinned"])
        XCTAssertNotNil(closed.documents["home"])
    }

    func testAnOpeningJoiningATransientFlightKeepsItsRealLeaseAndDocument() async throws {
        let peer = PickerViewPeer(hold: true)
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("home")
        var starts = await peer.starts().makeAsyncIterator()
        let loading = Task { try await session.load("issue_picker?q=shared") }
        let entered = await starts.next()
        XCTAssertNotNil(entered)
        let opening = await Self.openNow(in: session)
        await peer.release()
        let first = try await loading.value
        let opened = try await opening.value
        let snapshot = await session.currentSnapshot()
        let calls = await peer.calls
        await session.close(opened.lease)
        await session.stop()
        XCTAssertEqual(first, .document(try XCTUnwrap(opened.document)))
        XCTAssertEqual(snapshot.documents["issue_picker?q=shared"], opened.document)
        XCTAssertEqual(calls, 2, "home seed and one shared picker fetch")
    }

    func testConditionalOneOffPickerReturnsCompatibleContentWithoutGrowingCache() async throws {
        let peer = PickerViewPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        let key = "issue_picker?q=conditional"
        let first = try await session.load(key)
        await peer.useConditionalReplies()
        let second = try await session.load(key, known: "current")
        let snapshot = await session.currentSnapshot()
        let calls = await peer.calls
        await session.stop()
        switch second {
        case let .document(document): XCTAssertEqual(.document(document), first)
        case .unchanged:
            guard case let .document(document) = first else { return XCTFail("initial result must be a document") }
            XCTAssertEqual(document.version, "current")
        }
        XCTAssertEqual(snapshot.dataEpoch, "picker-epoch")
        XCTAssertNil(snapshot.documents[key])
        print("transient_picker_conditional calls=\(calls) cached_documents=\(snapshot.documents.count)")
    }

    func testTransientResultStillAdoptsANewEpochAndDropsOldCache() async throws {
        let peer = PickerViewPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("home")
        await peer.changeEpoch(to: "restored-picker-epoch")
        let result = try await session.load("issue_picker?q=restored")
        let snapshot = await session.currentSnapshot()
        await session.stop()
        guard case let .document(document) = result else { return XCTFail("the caller needs the actual restored result") }
        XCTAssertEqual(document.dataEpoch, "restored-picker-epoch")
        XCTAssertEqual(snapshot.dataEpoch, "restored-picker-epoch")
        XCTAssertTrue(snapshot.documents.isEmpty, "old-epoch home and unleased picker must not remain cached")
    }

    private static func openNow(in session: isolated FeedSession) -> Task<(lease: ViewLease, document: ViewDocument?), any Error> {
        Task.immediate { try await session.open("issue_picker?q=shared") }
    }
}

private actor PickerViewPeer: ViewTransport {
    private var hold: Bool
    private var conditional = false
    private var epoch = "picker-epoch"
    private var pending: (ViewFetch, CheckedContinuation<ViewFetch, any Error>)?
    private let stream: AsyncStream<Void>
    private let entered: AsyncStream<Void>.Continuation
    private(set) var calls = 0
    private(set) var returnedBytes = 0

    init(hold: Bool = false) {
        self.hold = hold
        let pair = AsyncStream<Void>.makeStream()
        stream = pair.stream; entered = pair.continuation
    }
    func starts() -> AsyncStream<Void> { stream }
    func useConditionalReplies() { conditional = true }
    func changeEpoch(to epoch: String) { self.epoch = epoch }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        calls += 1
        if conditional, known != nil { return .unchanged(cursor: "current") }
        let body: [String: Any] = ["protocol": 1, "view": key.hasPrefix("issue_picker") ? "issue_picker" : key,
            "key": key, "client": client.rawValue, "version": "current", "cursor": "current", "data_epoch": epoch,
            "status": ["version": "s", "summary": String(repeating: "x", count: 8192), "badge": 0,
                "counts": [], "gauges": [], "orchestrator": ["text": "idle", "tone": "neutral"]], "sections": []]
        let bytes = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        returnedBytes += bytes.count
        let result = try ContractCodec.decode(ViewFetch.self, from: bytes)
        if key == "issue_picker?q=shared", hold {
            hold = false
            return try await withCheckedThrowingContinuation { continuation in
                pending = (result, continuation); entered.yield(())
            }
        }
        return result
    }
    func release() {
        let completion = pending
        pending = nil
        if let completion { completion.1.resume(returning: completion.0) }
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed { throw CancellationError() }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse { throw CancellationError() }
}
