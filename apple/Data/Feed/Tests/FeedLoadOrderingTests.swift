import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

final class FeedLoadOrderingTests: XCTestCase {
    func testOlderSuccessAndMissingResponsesCannotReplaceANewerView() async throws {
        for missing in [false, true] {
            let transport = OrderedViewPeer()
            let session = FeedSession(transport: transport, client: .ios, lang: "en")
            var starts = await transport.starts().makeAsyncIterator()
            let older = Task { try await session.load("fleet") }
            let firstStart = await starts.next()
            let first = try XCTUnwrap(firstStart)
            let newer = Task { try await session.load("fleet", known: "previous") }
            let secondStart = await starts.next()
            let second = try XCTUnwrap(secondStart)
            await transport.complete(second, with: .success(try Self.document("new", cursor: "new-cursor")))
            _ = try await newer.value
            await transport.complete(first, with: missing ? .failure(FeedError.missingView) : .success(try Self.document("old", cursor: "old-cursor")))
            let olderResult = try await older.value
            XCTAssertEqual(olderResult, try Self.document("new", cursor: "new-cursor"), "a superseded caller must receive the current document")
            let snapshot = await session.currentSnapshot()
            XCTAssertEqual(snapshot.documents["fleet"]?.version, "new", "late \(missing ? "missing" : "success") response replaced the newer view")
            XCTAssertEqual(snapshot.cursor, "new-cursor")
            await session.stop()
        }
    }

    func testOverlappingEquivalentLoadsShareOneFetchAndReturnTheSameDocument() async throws {
        let expected = try Self.document("shared", cursor: "shared-cursor")
        let transport = SharedViewPeer(response: expected)
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        var starts = await transport.starts().makeAsyncIterator()
        let (first, second) = await Self.simultaneousLoads(in: session)
        let started = await starts.next()
        XCTAssertNotNil(started)
        await transport.release()
        let firstResult = try await first.value
        let secondResult = try await second.value
        XCTAssertEqual(firstResult, expected)
        XCTAssertEqual(secondResult, expected)
        let calls = await transport.callCount
        XCTAssertEqual(calls, 1, "equivalent overlapping requests should share the same transport fetch")
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.documents["fleet"]?.version, "shared")
        XCTAssertEqual(snapshot.cursor, "shared-cursor")
        await session.stop()
    }

    func testCancellingOneCallerKeepsTheSharedFetchForTheOtherCaller() async throws {
        let expected = try Self.document("shared", cursor: "shared-cursor")
        let transport = SharedViewPeer(response: expected)
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        var starts = await transport.starts().makeAsyncIterator()
        let (first, second) = await Self.simultaneousLoads(in: session)
        _ = await starts.next()
        first.cancel()
        switch await first.result {
        case let .failure(error): XCTAssertTrue(error is CancellationError)
        case .success: XCTFail("the cancelled caller must finish without waiting for the peer")
        }
        await transport.release()
        let remaining = try await second.value
        XCTAssertEqual(remaining, expected)
        let calls = await transport.callCount
        XCTAssertEqual(calls, 1)
        await session.stop()
    }

    func testConcurrentOpensPublishOneDocumentAndKeepIndependentLeases() async throws {
        let transport = SharedViewPeer(response: try Self.document("shared", cursor: "shared-cursor"))
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        let events = await session.events()
        var starts = await transport.starts().makeAsyncIterator()
        let (first, second) = await Self.simultaneousOpens(in: session)
        _ = await starts.next()
        await transport.release()
        let firstOpen = try await first.value
        let secondOpen = try await second.value
        XCTAssertEqual(firstOpen.document?.version, "shared")
        XCTAssertEqual(secondOpen.document?.version, "shared")
        XCTAssertNotEqual(firstOpen.lease, secondOpen.lease)
        await session.close(firstOpen.lease)
        let remaining = await session.currentSnapshot()
        XCTAssertEqual(remaining.documents["fleet"]?.version, "shared")
        await session.close(secondOpen.lease)
        let closed = await session.currentSnapshot()
        XCTAssertNil(closed.documents["fleet"])
        await session.stop()
        var publishedVersions: [String] = []
        for await event in events {
            if case let .snapshot(snapshot) = event, let document = snapshot.documents["fleet"] {
                publishedVersions.append(document.version)
            }
        }
        XCTAssertEqual(publishedVersions, ["shared"], "open must not republish the load's commit")
        let calls = await transport.callCount
        XCTAssertEqual(calls, 1)
    }

    func testEndingTheViewLifetimeDiscardsResponsesFromBeforeCloseOrStop() async throws {
        for stopping in [false, true] {
            let transport = OrderedViewPeer()
            let session = FeedSession(transport: transport, client: .ios, lang: "en")
            var starts = await transport.starts().makeAsyncIterator()
            let opening = Task { try await session.open("fleet") }
            let initialStart = await starts.next()
            let initial = try XCTUnwrap(initialStart)
            await transport.complete(initial, with: .success(try Self.document("seed", cursor: "seed-cursor")))
            let opened = try await opening.value
            let pending = Task { try await session.load("fleet", known: "seed") }
            let heldStart = await starts.next()
            let held = try XCTUnwrap(heldStart)
            if stopping { await session.stop() } else { await session.close(opened.lease) }
            await transport.complete(held, with: .success(try Self.document("late", cursor: "late-cursor")))
            switch await pending.result {
            case let .failure(error): XCTAssertTrue(error is CancellationError)
            case .success: XCTFail("a response from the ended lifetime must not be returned")
            }
            let snapshot = await session.currentSnapshot()
            XCTAssertNil(snapshot.documents["fleet"])
            let openKeys = await session.openViewKeys
            XCTAssertEqual(openKeys, [])
            await session.stop()
        }
    }

    func testResetMovesPendingCallersToOneFreshRequestAndKeepsTheFeedCursor() async throws {
        let transport = OrderedViewPeer(controlsChanges: true)
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        var events = await session.events().makeAsyncIterator()
        var starts = await transport.starts().makeAsyncIterator()
        var polls = await transport.changeStarts().makeAsyncIterator()
        let opening = Task { try await session.open("fleet") }
        let initialStart = await starts.next()
        let initial = try XCTUnwrap(initialStart)
        await transport.complete(initial, with: .success(try Self.document("seed", cursor: "seed-cursor")))
        let opened = try await opening.value
        let pollStart = await polls.next()
        let poll = try XCTUnwrap(pollStart)
        let pending = Task { try await session.load("fleet", known: "seed") }
        let oldStart = await starts.next()
        let old = try XCTUnwrap(oldStart)
        await transport.completeChange(poll, with: try Self.resetReply())
        let freshStart = await starts.next()
        let fresh = try XCTUnwrap(freshStart)
        await transport.complete(fresh, with: .success(try Self.document("new", cursor: "view-cursor", epoch: "new-epoch")))
        await transport.complete(old, with: .success(try Self.document("late", cursor: "late-cursor")))
        let resumed = try await pending.value
        XCTAssertEqual(resumed, try Self.document("new", cursor: "view-cursor", epoch: "new-epoch"))
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.documents["fleet"]?.version, "new")
        XCTAssertEqual(snapshot.cursor, "feed-cursor")
        XCTAssertEqual(snapshot.dataEpoch, "new-epoch")
        let conditions = await transport.conditions
        XCTAssertEqual(conditions, [nil, "seed", nil], "reset must discard the old condition and share the refresh request")
        while let event = await events.next() { if event == .reset { break } }
        await session.close(opened.lease)
        await session.stop()
        await transport.finishChanges()
    }

    func testCrossKeyEpochChangesCannotMixOldCachedOrReturnedDocuments() async throws {
        for seeded in [false, true] {
            let transport = OrderedViewPeer()
            let session = FeedSession(transport: transport, client: .ios, lang: "en")
            var starts = await transport.starts().makeAsyncIterator()
            if seeded {
                let seed = Task { try await session.load("home") }
                let start = await starts.next()
                let id = try XCTUnwrap(start)
                await transport.complete(id, with: .success(try Self.document("home-old", cursor: "old-cursor", epoch: "old-epoch", key: "home")))
                _ = try await seed.value
            }
            let fleet = Task { try await session.load("fleet") }
            let fleetStart = await starts.next()
            let fleetID = try XCTUnwrap(fleetStart)
            let tasks = Task { try await session.load("tasks") }
            let tasksStart = await starts.next()
            let tasksID = try XCTUnwrap(tasksStart)
            let freshFleet = try Self.document("fleet-new", cursor: "new-cursor", epoch: "new-epoch")
            await transport.serveImmediately("fleet", response: freshFleet)
            await transport.complete(tasksID, with: .success(try Self.document("tasks-new", cursor: "new-cursor", epoch: "new-epoch", key: "tasks")))
            _ = try await tasks.value
            await transport.complete(fleetID, with: .success(try Self.document("fleet-old", cursor: "old-cursor", epoch: "old-epoch")))
            let returnedFleet = try await fleet.value
            XCTAssertEqual(returnedFleet, freshFleet, "the caller must recover in the accepted session epoch")
            let snapshot = await session.currentSnapshot()
            XCTAssertEqual(snapshot.dataEpoch, "new-epoch")
            XCTAssertFalse(snapshot.documents.values.contains { $0.dataEpoch == "old-epoch" })
            XCTAssertEqual(snapshot.documents["fleet"]?.version, "fleet-new")
            XCTAssertEqual(snapshot.documents["tasks"]?.version, "tasks-new")
            await session.stop()
        }
    }

    func testUnchangedForAnotherVersionRecoversWithoutReturningTheOldCache() async throws {
        for overlapping in [false, true] {
            let transport = OrderedViewPeer()
            let session = FeedSession(transport: transport, client: .ios, lang: "en")
            var starts = await transport.starts().makeAsyncIterator()
            let seed = Task { try await session.load("fleet") }
            let seedStart = await starts.next()
            let seedID = try XCTUnwrap(seedStart)
            await transport.complete(seedID, with: .success(try Self.document("A", cursor: "A-cursor")))
            _ = try await seed.value
            var unconditional: Task<ViewFetch, any Error>?
            var unconditionalID: Int?
            if overlapping {
                unconditional = Task { try await session.load("fleet") }
                let start = await starts.next()
                unconditionalID = try XCTUnwrap(start)
            }
            let conditional = Task { try await session.load("fleet", known: "B") }
            let conditionalStart = await starts.next()
            let conditionalID = try XCTUnwrap(conditionalStart)
            let fresh = try Self.document("B", cursor: "B-cursor")
            await transport.serveImmediately("fleet", response: fresh)
            await transport.complete(conditionalID, with: .success(.unchanged(cursor: "B-cursor")))
            if let unconditionalID {
                await transport.complete(unconditionalID, with: .success(try Self.document("A", cursor: "A-cursor")))
            }
            let conditionalResult = try await conditional.value
            XCTAssertEqual(conditionalResult, fresh, "unchanged B cannot validate cached A")
            if let unconditional {
                let unconditionalResult = try await unconditional.value
                XCTAssertEqual(unconditionalResult, fresh, "a conditional mismatch must not cancel the unconditional recovery and return A")
            }
            let snapshot = await session.currentSnapshot()
            XCTAssertEqual(snapshot.documents["fleet"]?.version, "B")
            await session.stop()
        }
    }

    func testUnleasedFailedKeysReleaseTheirOwnerAndLifetimeBookkeeping() async throws {
        let session = FeedSession(transport: FailingViewPeer(), client: .ios, lang: "en")
        for index in 0..<32 {
            for missing in [false, true] {
                let key = "card:\(missing ? "missing" : "transient")-\(index)"
                do {
                    _ = try await session.load(key)
                    XCTFail("the failing peer must not return a document")
                } catch let error as FeedError {
                    XCTAssertTrue(missing)
                    XCTAssertEqual(error, .missingView)
                } catch let error as URLError {
                    XCTAssertFalse(missing)
                    XCTAssertEqual(error.code, .networkConnectionLost)
                }
            }
        }
        let snapshot = await session.currentSnapshot()
        XCTAssertTrue(snapshot.documents.isEmpty)
        let open = await session.openViewKeys
        XCTAssertEqual(open, [])
        let owners = await session.loadOwners.count
        let lifetimes = await session.loadLifetimes.count
        XCTAssertEqual(owners, 0, "failed one-off keys must not accumulate publication owners")
        XCTAssertEqual(lifetimes, 0, "failed one-off keys must not accumulate lifetime tokens")
        await session.stop()
    }

    func testAnOldFeedReplyCannotRollBackAnEpochDiscoveredByAView() async throws {
        let transport = OrderedViewPeer(controlsChanges: true)
        let session = FeedSession(transport: transport, client: .ios, lang: "en")
        let events = await session.events()
        var starts = await transport.starts().makeAsyncIterator()
        var polls = await transport.changeStarts().makeAsyncIterator()
        var operations = await transport.operations().makeAsyncIterator()
        let opening = Task { try await session.open("fleet") }
        let seedStart = await starts.next()
        let seed = try XCTUnwrap(seedStart)
        await transport.complete(seed, with: .success(try Self.document("fleet-old", cursor: "old-cursor", epoch: "old-epoch")))
        let opened = try await opening.value
        let firstPollStart = await polls.next()
        let firstPoll = try XCTUnwrap(firstPollStart)
        await transport.completeChange(firstPoll, with: try Self.feedReply(cursor: "old-feed", epoch: "old-epoch"))
        let oldPollStart = await polls.next()
        let oldPoll = try XCTUnwrap(oldPollStart)
        let tasks = Task { try await session.load("tasks") }
        let tasksStart = await starts.next()
        let tasksID = try XCTUnwrap(tasksStart)
        await transport.serveImmediately("fleet", response: try Self.document("fleet-new", cursor: "new-cursor", epoch: "new-epoch"))
        await transport.complete(tasksID, with: .success(try Self.document("tasks-new", cursor: "new-cursor", epoch: "new-epoch", key: "tasks")))
        _ = try await tasks.value
        while let operation = await operations.next() {
            if operation == .view(key: "tasks", known: nil) { break }
        }
        await transport.completeChange(oldPoll, with: try Self.feedReply(cursor: "old-feed", epoch: "old-epoch"))
        let nextOperation = await operations.next()
        XCTAssertEqual(nextOperation, .poll(cursor: "new-cursor"), "the old reply must be discarded and polling resume from the accepted epoch")
        let snapshot = await session.currentSnapshot()
        XCTAssertEqual(snapshot.dataEpoch, "new-epoch")
        XCTAssertEqual(snapshot.cursor, "new-cursor")
        XCTAssertEqual(snapshot.documents["tasks"]?.version, "tasks-new")
        XCTAssertFalse(snapshot.documents.values.contains { $0.dataEpoch == "old-epoch" })
        await session.close(opened.lease)
        await session.stop()
        await transport.finishChanges()
        for await _ in events { }
    }

    private static func feedReply(cursor: String, epoch: String) throws -> ViewFeed {
        let json = """
        {"protocol":1,"cursor":"\(cursor)","data_epoch":"\(epoch)","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"stale":[],"reset":false,"superseded":false}
        """
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    func testClosingTheLastLeaseCannotRefetchAfterAnotherKeyChangesEpoch() async throws {
        for completed in [false, true] {
            let transport = OrderedViewPeer()
            let session = FeedSession(transport: transport, client: .ios, lang: "en")
            var starts = await transport.starts().makeAsyncIterator()
            let opening = Task { try await session.open("fleet") }
            let seedStart = await starts.next()
            let seedID = try XCTUnwrap(seedStart)
            await transport.complete(seedID, with: .success(try Self.document("seed", cursor: "old-cursor", epoch: "old-epoch")))
            let opened = try await opening.value
            let fleet = Task { try await session.load("fleet", known: "seed") }
            let fleetStart = await starts.next()
            let fleetID = try XCTUnwrap(fleetStart)
            let tasks = Task { try await session.load("tasks") }
            let tasksStart = await starts.next()
            let tasksID = try XCTUnwrap(tasksStart)
            await transport.serveImmediately("fleet", response: try Self.document("refetched", cursor: "new-cursor", epoch: "new-epoch"))
            let newTasks = try Self.document("tasks-new", cursor: "new-cursor", epoch: "new-epoch", key: "tasks")
            let oldFleet = try Self.document("before-close", cursor: "old-cursor", epoch: "old-epoch")
            try await Self.closeBeforeEpochCompletion(in: session, lease: opened.lease,
                fleetResponse: completed ? oldFleet : nil, response: newTasks)
            switch await fleet.result {
            case let .failure(error): XCTAssertTrue(error is CancellationError)
            case .success: XCTFail("the ended lifetime must remain terminal after the epoch changes, including a queued success")
            }
            let tasksResult = try await tasks.value
            XCTAssertEqual(tasksResult, newTasks)
            await transport.complete(fleetID, with: .success(oldFleet))
            await transport.complete(tasksID, with: .success(newTasks))
            let keys = await transport.viewKeys
            XCTAssertEqual(keys.filter { $0 == "fleet" }.count, 2, "closing a lease must not create a recovery fetch")
            let snapshot = await session.currentSnapshot()
            XCTAssertNil(snapshot.documents["fleet"])
            XCTAssertEqual(snapshot.documents["tasks"]?.version, "tasks-new")
            XCTAssertEqual(snapshot.dataEpoch, "new-epoch")
            let open = await session.openViewKeys
            XCTAssertEqual(open, [])
            await session.stop()
        }
    }

    private static func closeBeforeEpochCompletion(in session: isolated FeedSession, lease: ViewLease,
        fleetResponse: ViewFetch?, response: ViewFetch) throws {
        let owner = try XCTUnwrap(session.loadOwners["tasks"])
        if let fleetResponse {
            let fleetOwner = try XCTUnwrap(session.loadOwners["fleet"])
            session.completeLoad(FeedSession.LoadKey(key: "fleet", known: "seed"), id: fleetOwner, result: .success(fleetResponse))
        }
        session.close(lease)
        session.completeLoad(FeedSession.LoadKey(key: "tasks", known: nil), id: owner, result: .success(response))
    }

    private static func simultaneousOpens(in session: isolated FeedSession) -> (Task<(lease: ViewLease, document: ViewDocument?), any Error>, Task<(lease: ViewLease, document: ViewDocument?), any Error>) {
        let first = Task.immediate { try await session.open("fleet") }
        let second = Task.immediate { try await session.open("fleet") }
        return (first, second)
    }

    private static func resetReply() throws -> ViewFeed {
        let json = """
        {"protocol":1,"cursor":"feed-cursor","data_epoch":"new-epoch","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"stale":[],"reset":true,"superseded":false}
        """
        return try ContractCodec.decode(ViewFeed.self, from: Data(json.utf8))
    }

    private static func simultaneousLoads(in session: isolated FeedSession) -> (Task<ViewFetch, any Error>, Task<ViewFetch, any Error>) {
        let first = Task.immediate { try await session.load("fleet") }
        let second = Task.immediate { try await session.load("fleet") }
        return (first, second)
    }

    private static func document(_ version: String, cursor: String, epoch: String = "epoch", key: String = "fleet") throws -> ViewFetch {
        let json = """
        {"protocol":1,"view":"\(key)","key":"\(key)","client":"ios","version":"\(version)",
        "cursor":"\(cursor)","data_epoch":"\(epoch)","status":{"version":"s","summary":"Ready","badge":0,
        "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }
}

private enum ViewPeerOperation: Equatable, Sendable {
    case view(key: String, known: String?)
    case poll(cursor: String?)
}

private actor OrderedViewPeer: ViewTransport {
    private var next = 0
    private var automatic: [String: ViewFetch] = [:]
    private(set) var conditions: [String?] = []
    private(set) var viewKeys: [String] = []
    private let controlsChanges: Bool
    private var changesClosed = false
    private var nextChange = 0
    private var pendingChanges: [Int: CheckedContinuation<ViewFeed, any Error>] = [:]
    private let changeStream: AsyncStream<Int>
    private let changeContinuation: AsyncStream<Int>.Continuation
    private let operationStream: AsyncStream<ViewPeerOperation>
    private let operationContinuation: AsyncStream<ViewPeerOperation>.Continuation
    private var pending: [Int: CheckedContinuation<ViewFetch, any Error>] = [:]
    private let stream: AsyncStream<Int>
    private let continuation: AsyncStream<Int>.Continuation

    init(controlsChanges: Bool = false) {
        self.controlsChanges = controlsChanges
        let pair = AsyncStream<Int>.makeStream()
        stream = pair.stream
        continuation = pair.continuation
        let changes = AsyncStream<Int>.makeStream()
        changeStream = changes.stream
        changeContinuation = changes.continuation
        let operations = AsyncStream<ViewPeerOperation>.makeStream()
        operationStream = operations.stream
        operationContinuation = operations.continuation
    }

    func starts() -> AsyncStream<Int> { stream }
    func changeStarts() -> AsyncStream<Int> { changeStream }
    func operations() -> AsyncStream<ViewPeerOperation> { operationStream }
    func serveImmediately(_ key: String, response: ViewFetch) { automatic[key] = response }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        let id = next
        next += 1
        conditions.append(known)
        viewKeys.append(key)
        operationContinuation.yield(.view(key: key, known: known))
        if let response = automatic[key] { return response }
        return try await withCheckedThrowingContinuation { completion in
            pending[id] = completion
            continuation.yield(id)
        }
    }

    func complete(_ id: Int, with result: Result<ViewFetch, any Error>) {
        pending.removeValue(forKey: id)!.resume(with: result)
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        guard controlsChanges, !changesClosed else { throw CancellationError() }
        operationContinuation.yield(.poll(cursor: cursor))
        let id = nextChange
        nextChange += 1
        return try await withCheckedThrowingContinuation { completion in
            pendingChanges[id] = completion
            changeContinuation.yield(id)
        }
    }

    func completeChange(_ id: Int, with reply: ViewFeed) {
        pendingChanges.removeValue(forKey: id)!.resume(returning: reply)
    }

    func finishChanges() {
        changesClosed = true
        let pending = pendingChanges.values
        pendingChanges.removeAll()
        for completion in pending { completion.resume(throwing: CancellationError()) }
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw CancellationError()
    }
}

private actor SharedViewPeer: ViewTransport {
    private let response: ViewFetch
    private(set) var callCount = 0
    private var released = false
    private var pending: [Int: CheckedContinuation<ViewFetch, any Error>] = [:]
    private let stream: AsyncStream<Int>
    private let continuation: AsyncStream<Int>.Continuation

    init(response: ViewFetch) {
        self.response = response
        let pair = AsyncStream<Int>.makeStream()
        stream = pair.stream
        continuation = pair.continuation
    }

    func starts() -> AsyncStream<Int> { stream }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        callCount += 1
        let id = callCount
        continuation.yield(id)
        if released { return response }
        return try await withTaskCancellationHandler {
            try Task.checkCancellation()
            return try await withCheckedThrowingContinuation { completion in
                if Task.isCancelled { completion.resume(throwing: CancellationError()) }
                else { pending[id] = completion }
            }
        } onCancel: {
            Task { await self.cancel(id) }
        }
    }

    func release() {
        released = true
        let waiters = pending.values
        pending.removeAll()
        for waiter in waiters { waiter.resume(returning: response) }
    }

    private func cancel(_ id: Int) {
        pending.removeValue(forKey: id)?.resume(throwing: CancellationError())
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw CancellationError()
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw CancellationError()
    }
}

private actor FailingViewPeer: ViewTransport {
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        if key.hasPrefix("card:missing-") { throw FeedError.missingView }
        throw URLError(.networkConnectionLost)
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw CancellationError()
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw CancellationError()
    }
}
