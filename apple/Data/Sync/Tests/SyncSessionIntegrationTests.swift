import AlondraContracts
import AlondraSync
import Foundation
import XCTest

private actor ControlledTransport: SyncTransport {
    enum Request: Equatable { case overview, changes(after: UInt64, seconds: Int) }
    private var requests: [Request] = []
    private var pending: [Int: CheckedContinuation<Data, Error>] = [:]
    private var requestWaiters: [(Int, CheckedContinuation<Void, Never>)] = []
    private var cancelled = 0
    private var cancellationWaiters: [(Int, CheckedContinuation<Void, Never>)] = []
    private let lateResponses: Set<Int>

    init(lateResponses: Set<Int> = []) { self.lateResponses = lateResponses }

    func fetchOverview() async throws -> Data { try await perform(.overview) }
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        _ = try await perform(.changes(after: afterSnapshotTick, seconds: seconds))
    }

    private func perform(_ request: Request) async throws -> Data {
        let index = requests.count
        requests.append(request)
        resumeRequestWaiters()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                if Task.isCancelled && !lateResponses.contains(index) {
                    recordCancellation()
                    continuation.resume(throwing: CancellationError())
                } else {
                    pending[index] = continuation
                }
            }
        } onCancel: { Task { await self.cancel(index) } }
    }

    func waitForRequests(_ count: Int) async {
        if requests.count >= count { return }
        await withCheckedContinuation { requestWaiters.append((count, $0)) }
    }
    func request(_ index: Int) -> Request { requests[index] }
    func requestCount() -> Int { requests.count }
    func respond(_ index: Int, body: String) {
        pending.removeValue(forKey: index)?.resume(returning: Data(body.utf8))
    }
    func fail(_ index: Int, error: any Error = URLError(.networkConnectionLost)) {
        pending.removeValue(forKey: index)?.resume(throwing: error)
    }
    func waitForCancellations(_ count: Int) async {
        if cancelled >= count { return }
        await withCheckedContinuation { cancellationWaiters.append((count, $0)) }
    }
    private func cancel(_ index: Int) {
        guard let continuation = pending[index] else { return }
        recordCancellation()
        guard !lateResponses.contains(index) else { return }
        pending[index] = nil
        continuation.resume(throwing: CancellationError())
    }
    private func recordCancellation() {
        cancelled += 1
        let ready = cancellationWaiters.filter { cancelled >= $0.0 }
        cancellationWaiters.removeAll { cancelled >= $0.0 }
        for (_, waiter) in ready { waiter.resume() }
    }
    private func resumeRequestWaiters() {
        let ready = requestWaiters.filter { requests.count >= $0.0 }
        requestWaiters.removeAll { requests.count >= $0.0 }
        for (_, waiter) in ready { waiter.resume() }
    }
}

private actor AlternateSyncTransport: SyncTransport {
    private let body: Data
    private var overviewCalls = 0
    private var pollTick: UInt64?
    private var pollWaiter: CheckedContinuation<Void, Never>?

    init(body: Data) { self.body = body }

    func fetchOverview() async throws -> Data {
        overviewCalls += 1
        return body
    }

    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        pollTick = afterSnapshotTick
        pollWaiter?.resume()
        pollWaiter = nil
        try await Task.sleep(for: .seconds(60))
    }

    func waitForPoll() async {
        if pollTick != nil { return }
        await withCheckedContinuation { pollWaiter = $0 }
    }

    func counts() -> (Int, UInt64?) { (overviewCalls, pollTick) }
}

private func nextSnapshot(from iterator: inout AsyncStream<SyncState>.Iterator) async -> SyncState? {
    while let state = await iterator.next() {
        if state.snapshot != nil { return state }
    }
    return nil
}

final class SyncSessionIntegrationTests: XCTestCase {
    private let base = URL(string: "https://alondra.example")!
    private func identity(_ revision: String, server: URL? = nil) -> SyncIdentity {
        SyncIdentity(server: server ?? base, deviceID: "device", credentialRevision: revision)
    }

    /// The fields match the current daemon's `/v1/overview` envelope, including the machine
    /// payload. Decode it through the same DTO the app uses before exercising the sync loop.
    private func overview(_ tick: UInt64) throws -> String {
        let body = """
        {"tick":\(tick),"seq":1,"nodes":[],"open_questions":[],"news":[],
         "orchestrator_messages":[],"recent_questions":[],"reports":[],"prs":[],
         "requests":[],"tasks":[],"user":"David","usage":null,
         "machine":{"at":0,"memory_total":1000,"memory_available":500,
                    "memory_free_percent":50,"load":[0,0,0],"cpus":8,"cpu_usage":0,
                    "disk_total":1000,"disk_available":500,"unreliable":false}}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        XCTAssertEqual(try decoder.decode(Overview.self, from: Data(body.utf8)).tick, tick)
        return body
    }

    func testAlternateTransportSharesOneSessionAcrossScenes() async throws {
        let relay = AlternateSyncTransport(body: Data(try overview(9).utf8))
        let identity = SyncIdentity(server: URL(string: "https://relay.example")!,
                                    deviceID: "phone-1", credentialRevision: "account-1/mac-1/key-1")
        let session = SyncSession(transport: relay, identity: identity)
        var first = await session.subscribe().makeAsyncIterator()
        var second = await session.subscribe().makeAsyncIterator()

        let firstSnapshot = await nextSnapshot(from: &first)
        let secondSnapshot = await nextSnapshot(from: &second)
        XCTAssertEqual(firstSnapshot?.snapshot?.tick, 9)
        XCTAssertEqual(firstSnapshot?.identity, identity)
        XCTAssertEqual(firstSnapshot?.publicationRevision, secondSnapshot?.publicationRevision)
        await relay.waitForPoll()
        let (overviewCalls, afterTick) = await relay.counts()
        XCTAssertEqual(overviewCalls, 1)
        XCTAssertEqual(afterTick, 9)
        await session.stop()
    }

    func testExplicitRefreshPreservesVisibleSnapshotAndUpdatesBothWindows() async throws {
        let transport = AlternateSyncTransport(body: Data(try overview(9).utf8))
        let identity = SyncIdentity(server: base, deviceID: "phone", credentialRevision: "approved")
        let session = SyncSession(transport: transport, identity: identity)
        var first = await session.subscribe().makeAsyncIterator()
        var second = await session.subscribe().makeAsyncIterator()
        let initial = await nextSnapshot(from: &first)
        _ = await nextSnapshot(from: &second)
        await transport.waitForPoll()
        await session.refresh()
        let visible = await session.state
        XCTAssertEqual(visible.snapshot, initial?.snapshot)
        let refreshed = await nextSnapshot(from: &first)
        let otherWindow = await nextSnapshot(from: &second)
        XCTAssertEqual(refreshed?.snapshot, initial?.snapshot)
        XCTAssertEqual(refreshed?.identity, identity)
        XCTAssertEqual(refreshed?.publicationRevision, otherWindow?.publicationRevision)
        XCTAssertGreaterThan(try XCTUnwrap(refreshed?.publicationRevision), try XCTUnwrap(initial?.publicationRevision))
        let counts = await transport.counts()
        XCTAssertEqual(counts.0, 2)
        await session.stop()
    }

    func testTwoWindowsShareOneLongPollAndStopCancelsIt() async throws {
        let transport = ControlledTransport()
        let session = SyncSession(transport: transport, identity: identity("local"))
        var first = await session.subscribe().makeAsyncIterator()
        var second = await session.subscribe().makeAsyncIterator()

        await transport.waitForRequests(1)
        let initialCount = await transport.requestCount()
        XCTAssertEqual(initialCount, 1)
        await transport.respond(0, body: try overview(7))
        let firstSnapshot = await nextSnapshot(from: &first)
        let secondSnapshot = await nextSnapshot(from: &second)
        XCTAssertEqual(firstSnapshot?.snapshot?.tick, 7)
        XCTAssertEqual(secondSnapshot?.snapshot?.tick, 7)

        await transport.waitForRequests(2)
        let poll = await transport.request(1)
        XCTAssertEqual(poll, .changes(after: 7, seconds: 10))
        let pollCount = await transport.requestCount()
        XCTAssertEqual(pollCount, 2)

        await session.stop()
        await transport.waitForCancellations(1)
    }

    func testIdenticalOverviewRefetchPublishesOneFreshRevisionToBothWindows() async throws {
        let transport = ControlledTransport()
        let session = SyncSession(transport: transport, identity: identity("local"))
        var first = await session.subscribe().makeAsyncIterator()
        var second = await session.subscribe().makeAsyncIterator()
        let body = try overview(7)

        await transport.waitForRequests(1)
        await transport.respond(0, body: body)
        let firstSnapshot = await nextSnapshot(from: &first)
        let secondSnapshot = await nextSnapshot(from: &second)
        let initialFirst = try XCTUnwrap(firstSnapshot)
        let initialSecond = try XCTUnwrap(secondSnapshot)
        XCTAssertEqual(initialFirst.publicationRevision, initialSecond.publicationRevision)

        await transport.waitForRequests(2)
        await transport.respond(1, body: #"{"tick":7}"#)
        await transport.waitForRequests(3)
        await transport.respond(2, body: body)
        let firstRefresh = await nextSnapshot(from: &first)
        let secondRefresh = await nextSnapshot(from: &second)
        let refreshedFirst = try XCTUnwrap(firstRefresh)
        let refreshedSecond = try XCTUnwrap(secondRefresh)

        XCTAssertEqual(refreshedFirst.snapshot, initialFirst.snapshot)
        XCTAssertEqual(refreshedFirst.publicationRevision, initialFirst.publicationRevision + 1)
        XCTAssertEqual(refreshedFirst.publicationRevision, refreshedSecond.publicationRevision)
        XCTAssertNotEqual(refreshedFirst, initialFirst,
                          "A fresh identical overview must pass the app projection cache and settle a pending push route")
        await transport.waitForRequests(4)
        let requestCount = await transport.requestCount()
        XCTAssertEqual(requestCount, 4, "Both windows must still share one long poll")
        await session.stop()
    }

    func testCredentialSwitchDiscardsLateOldOverviewAndStartsFreshPoll() async throws {
        let transport = ControlledTransport(lateResponses: [0])
        let old = identity("key-1")
        let new = identity("key-2")
        let session = SyncSession(transport: transport, identity: old)
        var observer = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)

        await session.use(transport: transport, identity: new)
        await transport.waitForRequests(2)
        let newRequest = await transport.request(1)
        XCTAssertEqual(newRequest, .overview)
        let activeIdentity = await session.identity
        XCTAssertEqual(activeIdentity, new)
        await transport.waitForCancellations(1)
        await transport.respond(0, body: try overview(99))
        await transport.respond(1, body: try overview(1))
        let newSnapshot = await nextSnapshot(from: &observer)
        XCTAssertEqual(newSnapshot?.snapshot?.tick, 1)
        XCTAssertEqual(newSnapshot?.identity.credentialRevision, "key-2")

        await transport.waitForRequests(3)
        let poll = await transport.request(2)
        XCTAssertEqual(poll, .changes(after: 1, seconds: 10))
        await session.stop()
    }

    func testLateUnauthorizedFromRetiredIdentityCannotUnpairCurrentSession() async throws {
        let transport = ControlledTransport(lateResponses: [0])
        let old = identity("key-1")
        let new = identity("key-2", server: URL(string: "https://new.example")!)
        let session = SyncSession(transport: transport, identity: old, isUnauthorized: { _ in
            XCTFail("The cancelled old read reached error presentation after identity replacement")
            return true
        })
        var observer = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        await session.use(transport: transport, identity: new, isUnauthorized: {
            ($0 as? URLError)?.code == .userAuthenticationRequired
        })
        await transport.waitForRequests(2)
        await transport.waitForCancellations(1)
        // The transport deliberately completes despite cancellation, as a late HTTP 401 can.
        await transport.fail(0, error: URLError(.userAuthenticationRequired))
        await transport.respond(1, body: try overview(2))
        let current = await nextSnapshot(from: &observer)
        XCTAssertEqual(current?.identity, new)
        XCTAssertEqual(current?.snapshot?.tick, 2)
        XCTAssertEqual(current?.connection, .online)
        await transport.waitForRequests(3)

        // Positive control: an unauthorized response belonging to the current identity MUST
        // unpair it. Ignoring every authentication error would not satisfy this regression.
        await transport.fail(2, error: URLError(.userAuthenticationRequired))
        let unauthorized = await observer.next()
        XCTAssertEqual(unauthorized?.identity, new)
        XCTAssertEqual(unauthorized?.connection, .unpaired)
        XCTAssertEqual(unauthorized?.snapshot?.tick, 2)
        await session.stop()
    }

    func testLowerTickAfterChangeIsAcceptedAsDaemonRestart() async throws {
        let transport = ControlledTransport()
        let session = SyncSession(transport: transport, identity: identity("local"))
        var observer = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        await transport.respond(0, body: try overview(42))
        let initial = await nextSnapshot(from: &observer)
        XCTAssertEqual(initial?.snapshot?.tick, 42)

        await transport.waitForRequests(2)
        let heldPoll = await transport.request(1)
        XCTAssertEqual(heldPoll, .changes(after: 42, seconds: 10))
        // The daemon may return the same tick when its bounded wait expires. Fetch a full
        // overview anyway: a restarted daemon could now be at a lower tick.
        await transport.respond(1, body: #"{"tick":42}"#)
        await transport.waitForRequests(3)
        await transport.respond(2, body: try overview(2))
        let restarted = await nextSnapshot(from: &observer)
        XCTAssertEqual(restarted?.snapshot?.tick, 2)
        await transport.waitForRequests(4)
        let poll = await transport.request(3)
        XCTAssertEqual(poll, .changes(after: 2, seconds: 10))
        await session.stop()
    }

    func testOfflineSubscriberReplaysSnapshotThenStatusAndIdentitySwitchClearsIt() async throws {
        let transport = ControlledTransport()
        let session = SyncSession(transport: transport, identity: identity("local"), retryDelay: .seconds(60))
        var first = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        await transport.respond(0, body: try overview(8))
        let initial = await nextSnapshot(from: &first)
        XCTAssertEqual(initial?.snapshot?.tick, 8)

        await transport.waitForRequests(2)
        await transport.fail(1)
        let offline = await first.next()
        guard case .offline = offline?.connection else { return XCTFail("Expected offline after transport failure") }
        XCTAssertEqual(offline?.snapshot?.tick, 8)

        var late = await session.subscribe().makeAsyncIterator()
        let replayed = await late.next()
        XCTAssertEqual(replayed?.snapshot?.tick, 8)
        guard case .offline = replayed?.connection else { return XCTFail("Expected current offline status") }

        let replacement = identity("other", server: URL(string: "https://new.example")!)
        await session.use(transport: transport, identity: replacement)
        var switched = await session.subscribe().makeAsyncIterator()
        let connecting = await switched.next()
        guard case .connecting = connecting?.connection else { return XCTFail("Expected connecting on new identity") }
        XCTAssertNil(connecting?.snapshot, "Old snapshot leaked across identity")
        XCTAssertEqual(connecting?.identity.server, replacement.server)
        await transport.waitForRequests(3)
        await transport.respond(2, body: try overview(1))
        let fresh = await nextSnapshot(from: &switched)
        XCTAssertEqual(fresh?.snapshot?.tick, 1)
        await session.stop()
    }

    func testSlowWindowNeverReceivesPreviousIdentityAfterRepeatedNewServerFailures() async throws {
        let transport = ControlledTransport()
        let old = identity("old")
        let new = identity("new", server: URL(string: "https://new.example")!)
        let session = SyncSession(transport: transport, identity: old, retryDelay: .milliseconds(1))
        var slow = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        await transport.respond(0, body: try overview(80))
        let oldState = await nextSnapshot(from: &slow)
        XCTAssertEqual(oldState?.snapshot?.tick, 80)

        // The window stops reading while the session changes identity and the new server
        // repeatedly fails. The reset event itself may be evicted from the one-state buffer.
        await session.use(transport: transport, identity: new)
        for index in 1...3 {
            await transport.waitForRequests(index + 1)
            await transport.fail(index)
        }
        await transport.waitForRequests(5)
        let resumed = await slow.next()
        XCTAssertEqual(resumed?.identity.server, new.server)
        XCTAssertEqual(resumed?.identity.credentialRevision, "new")
        XCTAssertNil(resumed?.snapshot, "A's snapshot must never be rendered as B's state")
        guard case .offline = resumed?.connection else { return XCTFail("Expected B offline") }
        await session.stop()
    }

    func testInvalidServerSuspendsOldRequestAndExistingWindowsResumeOnValidServer() async throws {
        let transport = ControlledTransport()
        let old = identity("old")
        let replacement = identity("other", server: URL(string: "https://new.example")!)
        let session = SyncSession(transport: transport, identity: old)
        var window = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)

        await session.suspendForInvalidConfiguration("Invalid server URL")
        await transport.waitForCancellations(1)
        let suspended = await window.next()
        guard case .invalidConfiguration = suspended?.connection else { return XCTFail("Expected invalid configuration") }
        XCTAssertNil(suspended?.snapshot)
        let pausedRequestCount = await transport.requestCount()
        XCTAssertEqual(pausedRequestCount, 1)

        await session.use(transport: transport, identity: replacement)
        await transport.waitForRequests(2)
        await transport.respond(1, body: try overview(1))
        let fresh = await nextSnapshot(from: &window)
        XCTAssertEqual(fresh?.identity.server, replacement.server)
        XCTAssertEqual(fresh?.snapshot?.tick, 1)
        await session.stop()
    }

    func testInvalidSavedServerNeverStartsTransportUntilReconfigured() async throws {
        let transport = ControlledTransport()
        let session = SyncSession(transport: transport, identity: identity("local"), initialInvalidConfiguration: "Invalid server URL")
        var window = await session.subscribe().makeAsyncIterator()
        let initial = await window.next()
        guard case .invalidConfiguration = initial?.connection else { return XCTFail("Expected invalid configuration") }
        XCTAssertNil(initial?.snapshot)
        let initialRequestCount = await transport.requestCount()
        XCTAssertEqual(initialRequestCount, 0)
        await session.use(transport: transport, identity: identity("local"))
        await transport.waitForRequests(1)
        await session.stop()
    }

    func testInvalidConfigurationSurvivesClosingAndReopeningLastWindow() async throws {
        let transport = ControlledTransport()
        let session = SyncSession(
            transport: transport,
            identity: identity("local"),
            initialInvalidConfiguration: "Invalid server URL"
        )
        let firstWindow = Task {
            for await _ in await session.subscribe() { }
        }
        await Task.yield()
        firstWindow.cancel()
        await firstWindow.value
        for _ in 0..<100 {
            if await session.state.generation > 0 { break }
            await Task.yield()
        }

        var reopened = await session.subscribe().makeAsyncIterator()
        let replayed = await reopened.next()
        guard case .invalidConfiguration = replayed?.connection else {
            return XCTFail("Reopening a scene must not restart the old server")
        }
        let requestCount = await transport.requestCount()
        XCTAssertEqual(requestCount, 0)
        await session.stop()
    }
}
