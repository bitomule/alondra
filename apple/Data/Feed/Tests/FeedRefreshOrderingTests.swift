import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class FeedRefreshOrderingTests: XCTestCase {
    func testAnIndependentViewIsVisibleWhileTheFirstRefreshRemainsPending() async throws {
        let transport = RefreshViewPeer()
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        let schedules = try await session.open("schedules")
        let tasks = try await session.open("tasks")
        await transport.beginRefresh()
        let freshTasks = try refreshDocument("tasks", version: "new")
        let (refresh, beforeRelease) = try await Self.startRefreshAndCompleteTasks(in: session, response: freshTasks)
        await transport.releaseSchedules()
        try await refresh.value
        XCTAssertEqual(beforeRelease.documents["schedules"]?.version, "seed", "the first key remains pending at the observation boundary")
        XCTAssertEqual(beforeRelease.documents["tasks"]?.version, "new", "the independent key must be published before the first key is released")
        let completed = await session.currentSnapshot()
        XCTAssertEqual(completed.documents["schedules"]?.version, "new")
        XCTAssertEqual(completed.documents["tasks"]?.version, "new")
        XCTAssertEqual(completed.dataEpoch, "epoch")
        await session.close(schedules.lease)
        await session.close(tasks.lease)
        await session.stop()
        await transport.finish()
    }

    func testAllViewLoadsShareFourSlotsAndEquivalentCallersShareOneSlot() async throws {
        let transport = BudgetViewPeer()
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        _ = try await session.load("home")
        let (loads, reserved) = await Self.startBudgetLoads(in: session)
        await transport.release()
        for load in loads { _ = try await load.value }
        XCTAssertEqual(reserved, 4, "foreground loads must use the same bounded budget as refresh and recovery")
        let calls = await transport.keys
        XCTAssertEqual(calls.filter { $0.hasPrefix("card:budget-") }.count, 6, "two equivalent callers must share one actual fetch")
        XCTAssertEqual(calls.filter { $0 == "card:budget-0" }.count, 1)
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0, "real transport completion must return every reserved slot")
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.documents.keys.filter { $0.hasPrefix("card:budget-") }.count, 6)
        await session.stop()
    }

    func testAnAlreadyCancelledRefreshStartsNoTransports() async throws {
        let transport = BudgetViewPeer(holding: false)
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        _ = try await session.open("schedules")
        _ = try await session.open("tasks")
        let before = await transport.keys
        let refresh = await Self.cancelBeforeRefreshEntry(in: session)
        _ = await refresh.result
        let after = await transport.keys
        XCTAssertEqual(after, before, "a cancelled refresh must not spawn independent uncancelled loads")
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0)
        await session.stop()
    }

    private static func cancelBeforeRefreshEntry(in session: isolated FeedSession) -> Task<Void, any Error> {
        let current = session.generation
        let task = Task { try await session.refresh(["schedules", "tasks"], generation: current) }
        task.cancel()
        return task
    }

    func testQueuedCancellationSkipsTheFetchAndOneSharedConsumerCanRemain() async throws {
        let transport = BudgetViewPeer()
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        _ = try await session.load("home")
        let (loads, _) = await Self.startBudgetLoads(in: session)
        let handles = await Array(session.runningLoadTasks.values)
        for index in [4, 5, 6] {
            loads[index].cancel()
            switch await loads[index].result {
            case let .failure(error): XCTAssertTrue(error is CancellationError)
            case .success: XCTFail("cancelled callers must finish while the peer remains held")
            }
        }
        let held = await session.runningLoadIDs.count
        await transport.release()
        for handle in handles { await handle.value }
        for index in 0..<4 { _ = try await loads[index].value }
        XCTAssertEqual(held, 4)
        let calls = await transport.keys.filter { $0.hasPrefix("card:budget-") }
        XCTAssertEqual(Set(calls), Set((0..<4).map { "card:budget-\($0)" }), "queued cancelled keys must never reach the transport")
        XCTAssertEqual(calls.filter { $0 == "card:budget-0" }.count, 1, "cancelling one shared caller must keep the other's fetch")
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0)
        await session.stop()
    }

    func testStopOrLastLeaseCloseDoesNotRefillFromEndedLifetimes() async throws {
        for stopping in [false, true] {
            let transport = BudgetViewPeer(holding: false)
            let session = FeedSession(transport: transport, client: .ios, lang: "en")
            var leases: [ViewLease] = []
            for index in 0..<6 { leases.append(try await session.open("card:budget-\(index)").lease) }
            await transport.hold()
            var starts = await transport.starts().makeAsyncIterator()
            let loads = await Self.startConditionalLoads(in: session)
            for _ in 0..<4 { let start = await starts.next(); XCTAssertNotNil(start) }
            let (handles, retained) = await Self.endLifetimes(in: session, leases: leases, stopping: stopping)
            for index in stopping ? Array(0..<6) : [0, 4] {
                switch await loads[index].result {
                case let .failure(error): XCTAssertTrue(error is CancellationError)
                case .success: XCTFail("an ended lifetime must remain terminal")
                }
            }
            await transport.release()
            for handle in handles { await handle.value }
            if !stopping { for index in [1, 2, 3, 5] { _ = try await loads[index].value } }
            XCTAssertEqual(retained, 4, "cancellation must keep slots owned by transports that are still alive")
            let requests = await transport.requests.filter { $0.known == "new" }
            XCTAssertFalse(requests.contains { $0.key == "card:budget-4" }, "last-lease close must remove a queued fetch")
            if stopping { XCTAssertFalse(requests.contains { $0.key == "card:budget-5" }, "stop must not refill queued work") }
            let snapshot = await session.currentSnapshot()
            XCTAssertNil(snapshot.documents["card:budget-0"])
            XCTAssertNil(snapshot.documents["card:budget-4"])
            if stopping { XCTAssertTrue(snapshot.documents.isEmpty) }
            let remaining = await session.runningLoadIDs.count
            XCTAssertEqual(remaining, 0)
            await session.stop()
        }
    }

    func testResetRecoveryUsesTheSameSlotsUntilOldTransportsActuallyExit() async throws {
        let transport = BudgetViewPeer(holding: false, controlsChanges: true)
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        _ = try await session.open("schedules")
        _ = try await session.open("tasks")
        var events = await session.events().makeAsyncIterator()
        var polls = await transport.polls().makeAsyncIterator()
        let initialPoll: Void? = await polls.next()
        XCTAssertNotNil(initialPoll)
        await transport.hold()
        var starts = await transport.starts().makeAsyncIterator()
        let loads = await Self.startFourLoads(in: session)
        for _ in 0..<4 { let start = await starts.next(); XCTAssertNotNil(start) }
        let oldHandles = await Array(session.runningLoadTasks.values)
        try await transport.resetPoll()
        while let event = await events.next() { if event == .reset { break } }
        let beforeRelease = await session.currentSnapshot()
        let held = await session.runningLoadIDs.count
        let callsBeforeRelease = await transport.keys.count
        await transport.release()
        for handle in oldHandles { await handle.value }
        for load in loads {
            let result = try await load.value
            guard case let .document(document) = result else { XCTFail("reset callers must receive a fresh document"); continue }
            XCTAssertEqual(document.dataEpoch, "new-epoch")
        }
        let recoveredPoll: Void? = await polls.next()
        XCTAssertNotNil(recoveredPoll, "the feed must recover and return to polling")
        XCTAssertEqual(held, 4, "reset cannot release slots while cancelled old transports are still held")
        XCTAssertEqual(callsBeforeRelease, 6, "recovery/refresh must queue behind the four live old transports")
        XCTAssertTrue(beforeRelease.documents.isEmpty)
        XCTAssertEqual(beforeRelease.dataEpoch, "new-epoch")
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.documents.count, 6)
        XCTAssertTrue(snapshot.documents.values.allSatisfy { $0.dataEpoch == "new-epoch" })
        XCTAssertEqual(snapshot.cursor, "feed-cursor")
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0)
        await session.stop()
        await transport.finishChanges()
    }

    func testCancellingAnEnteredRefreshKeepsTheForegroundConsumerAndOwnsItsChildren() async throws {
        let transport = BudgetViewPeer(holding: false)
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        let schedules = try await session.open("schedules")
        let tasks = try await session.open("tasks")
        await transport.hold(version: "late")
        var starts = await transport.starts().makeAsyncIterator()
        let (refresh, foreground, handles) = await Self.startSharedRefresh(in: session)
        for _ in handles.indices { let started = await starts.next(); XCTAssertNotNil(started) }
        refresh.cancel()
        let cancelled = await refresh.result
        let retained = await session.runningLoadIDs.count
        await session.close(tasks.lease)
        await transport.release()
        for handle in handles { await handle.value }
        let returned = try await foreground.value
        switch cancelled {
        case let .failure(error): XCTAssertTrue(error is CancellationError)
        case .success: XCTFail("an entered refresh must observe its parent's cancellation")
        }
        XCTAssertEqual(handles.count, 2, "both refresh children must be alive at the cancellation boundary")
        XCTAssertEqual(retained, 2, "a cancelled child keeps its slot until the held transport actually exits")
        guard case let .document(document) = returned else { XCTFail("the independent consumer must receive its document"); await session.stop(); return }
        XCTAssertEqual(document.version, "late")
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.documents["schedules"]?.version, "late")
        XCTAssertNil(snapshot.documents["tasks"], "the group's cancelled child cannot repopulate a closed key with its late response")
        let requests = await transport.requests.filter { $0.known == "new" }
        XCTAssertEqual(requests.filter { $0.key == "schedules" }.count, 1)
        XCTAssertEqual(requests.filter { $0.key == "tasks" }.count, 1)
        let remaining = await session.runningLoadIDs.count
        XCTAssertEqual(remaining, 0)
        await session.close(schedules.lease)
        await session.stop()
    }

    private static func startSharedRefresh(in session: isolated FeedSession) -> (Task<Void, any Error>, Task<ViewFetch, any Error>, [Task<Void, Never>]) {
        let current = session.generation
        let refresh = Task.immediate { try await session.refresh(["schedules", "tasks"], generation: current) }
        let foreground = Task.immediate { try await session.load("schedules", known: "new") }
        return (refresh, foreground, Array(session.runningLoadTasks.values))
    }

    private static func startFourLoads(in session: isolated FeedSession) -> [Task<ViewFetch, any Error>] {
        (0..<4).map { index in Task.immediate { try await session.load("card:budget-\(index)") } }
    }

    private static func startConditionalLoads(in session: isolated FeedSession) -> [Task<ViewFetch, any Error>] {
        (0..<6).map { index in Task.immediate { try await session.load("card:budget-\(index)", known: "new") } }
    }

    private static func endLifetimes(in session: isolated FeedSession, leases: [ViewLease], stopping: Bool) -> ([Task<Void, Never>], Int) {
        let handles = Array(session.runningLoadTasks.values)
        if stopping { session.stop() }
        else { session.close(leases[0]); session.close(leases[4]) }
        return (handles, session.runningLoadIDs.count)
    }

    private static func startBudgetLoads(in session: isolated FeedSession) -> ([Task<ViewFetch, any Error>], Int) {
        var loads: [Task<ViewFetch, any Error>] = []
        for index in 0..<6 {
            loads.append(Task.immediate { try await session.load("card:budget-\(index)") })
        }
        loads.append(Task.immediate { try await session.load("card:budget-0") })
        return (loads, session.runningLoadIDs.count)
    }

    private static func startRefreshAndCompleteTasks(in session: isolated FeedSession, response: ViewFetch) throws -> (Task<Void, any Error>, FeedSnapshot) {
        let prior = session.loadOwners["tasks"]
        let current = session.generation
        let refresh = Task.immediate { try await session.refresh(["schedules", "tasks"], generation: current) }
        if let owner = session.loadOwners["tasks"], owner != prior {
            session.completeLoad(FeedSession.LoadKey(key: "tasks", known: "seed"), id: owner, result: .success(response))
        }
        return (refresh, session.currentSnapshot())
    }
}

private func refreshDocument(_ key: String, version: String, epoch: String = "epoch") throws -> ViewFetch {
    let json = """
    {"protocol":1,"view":"\(key)","key":"\(key)","client":"ios","version":"\(version)",
    "cursor":"view-cursor","data_epoch":"\(epoch)","status":{"version":"s","summary":"Ready","badge":0,
    "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
    """
    return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
}

private actor RefreshViewPeer: ViewTransport {
    private var refreshing = false
    private var released = false
    private var schedulesWaiters: [CheckedContinuation<Void, Never>] = []

    func beginRefresh() { refreshing = true }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        if refreshing, key == "schedules", !released {
            await withCheckedContinuation { schedulesWaiters.append($0) }
        }
        return try refreshDocument(key, version: refreshing ? "new" : "seed")
    }

    func releaseSchedules() {
        released = true
        let waiters = schedulesWaiters
        schedulesWaiters.removeAll()
        for waiter in waiters { waiter.resume() }
    }

    func finish() { releaseSchedules() }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw CancellationError()
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw CancellationError()
    }
}

private struct RefreshRequest: Sendable {
    let key: String
    let known: String?
}

private actor BudgetViewPeer: ViewTransport {
    private var holding: Bool
    private var released = false
    private var epoch = "epoch"
    private var version = "new"
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private(set) var requests: [RefreshRequest] = []
    private let stream: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private let controlsChanges: Bool
    private var changesClosed = false
    private var pendingChanges: [CheckedContinuation<ViewFeed, any Error>] = []
    private let pollStream: AsyncStream<Void>
    private let pollContinuation: AsyncStream<Void>.Continuation

    init(holding: Bool = true, controlsChanges: Bool = false) {
        self.holding = holding
        self.controlsChanges = controlsChanges
        (stream, continuation) = AsyncStream<String>.makeStream()
        (pollStream, pollContinuation) = AsyncStream<Void>.makeStream()
    }

    var keys: [String] { requests.map(\.key) }
    func starts() -> AsyncStream<String> { stream }
    func polls() -> AsyncStream<Void> { pollStream }
    func hold(version: String = "new") { holding = true; released = false; self.version = version }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        requests.append(RefreshRequest(key: key, known: known))
        let response = try refreshDocument(key, version: version, epoch: epoch)
        if key != "home", holding, !released {
            await withCheckedContinuation { completion in
                waiters.append(completion)
                continuation.yield(key)
            }
        }
        return response
    }

    func release() {
        released = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        guard controlsChanges, !changesClosed else { throw CancellationError() }
        return try await withCheckedThrowingContinuation { completion in
            pendingChanges.append(completion)
            pollContinuation.yield(())
        }
    }

    func resetPoll() throws {
        epoch = "new-epoch"
        let json = """
        {"protocol":1,"cursor":"feed-cursor","data_epoch":"new-epoch","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"stale":[],"reset":true,"superseded":false}
        """
        let reply = try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
        pendingChanges.removeFirst().resume(returning: reply)
    }

    func finishChanges() {
        changesClosed = true
        let pending = pendingChanges
        pendingChanges.removeAll()
        for completion in pending { completion.resume(throwing: CancellationError()) }
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw CancellationError()
    }
}
