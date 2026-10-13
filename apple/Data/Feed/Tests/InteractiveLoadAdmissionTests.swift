import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class InteractiveLoadAdmissionTests: XCTestCase {
    func testBackgroundRefreshLeavesRoomForTheNextCardOnBothClients() async throws {
        for client in [ViewClient.ios, .mac] {
            let peer = AdmissionPeer()
            let session = FeedSession(transport: peer, client: client, lang: "en")
            var leases: [ViewLease] = []
            for index in 0..<5 { leases.append(try await session.open("card:refresh-\(index)").lease) }
            await peer.hold()
            let (refresh, detail, occupied, detailAdmitted) = await Self.refreshAndOpen(in: session)
            await peer.releaseAll()
            try await refresh.value
            let result = try await detail.value
            XCTAssertEqual(occupied, 4)
            XCTAssertTrue(detailAdmitted, "opening a card must not wait for any background refresh to return")
            guard case let .document(document) = result else { XCTFail("missing detail"); continue }
            XCTAssertEqual(document.key, "card:next")
            XCTAssertEqual(document.version, "fresh")
            let remaining = await session.runningLoadIDs.count
            XCTAssertEqual(remaining, 0)
            for lease in leases { await session.close(lease) }
            await session.stop()
        }
    }

    func testInteractiveJoinPromotesTheQueuedSharedFetchWithoutDuplicatingIt() async throws {
        let peer = AdmissionPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("home")
        await peer.hold()
        let (loads, joined, wasAdmitted) = await Self.promoteSharedLoad(in: session)
        await peer.releaseAll()
        for load in loads { _ = try await load.value }
        let result = try await joined.value
        XCTAssertTrue(wasAdmitted, "the interactive consumer must promote the shared queued request")
        guard case let .document(document) = result else { XCTFail("missing shared detail"); return }
        XCTAssertEqual(document.key, "card:background-3")
        let keys = await peer.keys
        XCTAssertEqual(keys.filter { $0 == "card:background-3" }.count, 1)
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0)
        await session.stop()
    }

    func testCancelledPhysicalBackgroundFetchesKeepTheirSlotsUntilTheyExit() async throws {
        let peer = AdmissionPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("home")
        await peer.hold()
        var starts = await peer.starts.makeAsyncIterator()
        let loads = await Self.backgroundLoads(in: session)
        for _ in 0..<3 { _ = await starts.next() }
        for task in loads { task.cancel() }
        for task in loads {
            switch await task.result {
            case let .failure(error): XCTAssertTrue(error is CancellationError)
            case .success: XCTFail("cancelled callers must not receive a late result")
            }
        }
        let (replacement, detail, occupied, backgroundAdmitted, detailAdmitted, handles) = await Self.afterCancellation(in: session)
        await peer.releaseAll()
        for handle in handles { await handle.value }
        _ = try await replacement.value
        _ = try await detail.value
        XCTAssertEqual(occupied, 4)
        XCTAssertFalse(backgroundAdmitted, "cancellation cannot create a fourth physical background slot")
        XCTAssertTrue(detailAdmitted)
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0)
        await session.stop()
    }

    func testWaitingBackgroundWorkProgressesDuringInteractiveTraffic() async throws {
        let peer = AdmissionPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("home")
        await peer.hold()
        var starts = await peer.starts.makeAsyncIterator()
        let (loads, handles) = await Self.queueMixedWork(in: session)
        for _ in 0..<4 { _ = await starts.next() }
        await peer.release("card:interactive-0")
        await handles[0].value
        let next = await starts.next()
        await peer.releaseAll()
        for load in loads { _ = try await load.value }
        XCTAssertEqual(next, "card:background", "three interactive admissions must give an eligible background request a turn")
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0)
        await session.stop()
    }

    func testCancellingAQueuedInteractiveJoinReturnsTheRequestToBackground() async throws {
        let peer = AdmissionPeer()
        let session = FeedSession(transport: peer, client: .ios, lang: "en")
        _ = try await session.load("home")
        await peer.hold()
        var starts = await peer.starts.makeAsyncIterator()
        let (loads, join, interactiveHandle) = await Self.queueThenJoin(in: session)
        for _ in 0..<4 { _ = await starts.next() }
        join.cancel()
        switch await join.result {
        case let .failure(error): XCTAssertTrue(error is CancellationError)
        case .success: XCTFail("the cancelled join must not receive the document")
        }
        await peer.release("card:held-interactive")
        await interactiveHandle?.value
        let backgroundAdmitted = await Self.admitted("card:background-3", in: session)
        let (detail, detailAdmitted) = await Self.openNext(in: session)
        await peer.releaseAll()
        for load in loads { _ = try await load.value }
        _ = try await detail.value
        XCTAssertFalse(backgroundAdmitted, "a cancelled interactive join cannot authorize a fourth background fetch")
        XCTAssertTrue(detailAdmitted, "the next real interactive consumer must retain the reserved slot")
        await session.stop()
    }

    private static func queueThenJoin(in session: isolated FeedSession) -> ([Task<ViewFetch, any Error>], Task<ViewFetch, any Error>, Task<Void, Never>?) {
        var loads = backgroundLoads(in: session)
        loads.append(Task.immediate { try await session.load("card:held-interactive") })
        let handle: Task<Void, Never>?
        if let id = session.loadOwners["card:held-interactive"] { handle = session.runningLoadTasks[id] } else { handle = nil }
        let join = Task.immediate { try await session.load("card:background-3") }
        return (loads, join, handle)
    }

    private static func openNext(in session: isolated FeedSession) -> (Task<ViewFetch, any Error>, Bool) {
        let detail = Task.immediate { try await session.load("card:next") }
        return (detail, admitted("card:next", in: session))
    }

    private static func refreshAndOpen(in session: isolated FeedSession) -> (Task<Void, any Error>, Task<ViewFetch, any Error>, Int, Bool) {
        let current = session.generation
        let refresh = Task.immediate { try await session.refresh((0..<5).map { "card:refresh-\($0)" }, generation: current) }
        let detail = Task.immediate { try await session.load("card:next") }
        let wasAdmitted = admitted("card:next", in: session)
        return (refresh, detail, session.runningLoadIDs.count, wasAdmitted)
    }

    private static func admitted(_ key: String, in session: isolated FeedSession) -> Bool {
        guard let id = session.loadOwners[key] else { return false }
        return session.runningLoadIDs.contains(id)
    }

    private static func backgroundLoads(in session: isolated FeedSession) -> [Task<ViewFetch, any Error>] {
        var loads: [Task<ViewFetch, any Error>] = []
        for index in 0..<4 {
            loads.append(Task.immediate { try await session.load("card:background-\(index)", known: nil, priority: .background) })
        }
        return loads
    }

    private static func promoteSharedLoad(in session: isolated FeedSession) -> ([Task<ViewFetch, any Error>], Task<ViewFetch, any Error>, Bool) {
        let loads = backgroundLoads(in: session)
        let joined = Task.immediate { try await session.load("card:background-3") }
        let wasAdmitted = admitted("card:background-3", in: session)
        return (loads, joined, wasAdmitted)
    }

    private static func afterCancellation(in session: isolated FeedSession) -> (Task<ViewFetch, any Error>, Task<ViewFetch, any Error>, Int, Bool, Bool, [Task<Void, Never>]) {
        let replacement = Task.immediate { try await session.load("card:replacement", known: nil, priority: .background) }
        let detail = Task.immediate { try await session.load("card:next") }
        let background = admitted("card:replacement", in: session)
        let interactive = admitted("card:next", in: session)
        return (replacement, detail, session.runningLoadIDs.count, background, interactive, Array(session.runningLoadTasks.values))
    }

    private static func queueMixedWork(in session: isolated FeedSession) -> ([Task<ViewFetch, any Error>], [Task<Void, Never>]) {
        var loads: [Task<ViewFetch, any Error>] = []
        var handles: [Task<Void, Never>] = []
        for index in 0..<4 {
            loads.append(Task.immediate { try await session.load("card:interactive-\(index)") })
            handles.append(session.runningLoadTasks[session.loadOwners["card:interactive-\(index)"]!]!)
        }
        loads.append(Task.immediate { try await session.load("card:interactive-4") })
        loads.append(Task.immediate { try await session.load("card:background", known: nil, priority: .background) })
        return (loads, handles)
    }
}

private actor AdmissionPeer: ViewTransport {
    nonisolated let starts: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private var holding = false
    private var waiters: [String: CheckedContinuation<Void, Never>] = [:]
    private(set) var keys: [String] = []

    init() { (starts, continuation) = AsyncStream.makeStream() }
    func hold() { holding = true }
    func release(_ key: String) { waiters.removeValue(forKey: key)?.resume() }
    func releaseAll() {
        holding = false
        let pending = waiters
        waiters.removeAll()
        for waiter in pending.values { waiter.resume() }
    }
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        keys.append(key)
        if holding {
            await withCheckedContinuation {
                waiters[key] = $0
                continuation.yield(key)
            }
        }
        let json = """
        {"protocol":1,"view":"\(key)","key":"\(key)","client":"\(client.rawValue)","version":"fresh",
        "cursor":"c","data_epoch":"e","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed { throw CancellationError() }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse { throw CancellationError() }
}
