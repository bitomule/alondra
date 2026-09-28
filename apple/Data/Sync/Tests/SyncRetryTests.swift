import AlondraContracts
import AlondraSync
import Foundation
import XCTest

final class SyncRetryTests: XCTestCase {
    func testBackoffDoublesFromOneSecondToAMinuteWithJitter() {
        let backoff = ConnectionBackoff.standard
        XCTAssertEqual((0..<9).map { backoff.delay(attempt: $0, random: 1) },
                       [1, 2, 4, 8, 16, 32, 60, 60, 60].map { Duration.seconds($0) })
        XCTAssertEqual(backoff.delay(attempt: 3, random: 0), .seconds(4))
        XCTAssertEqual(backoff.delay(attempt: 3, random: 0.5), .seconds(6))
        XCTAssertEqual(backoff.delay(attempt: 500, random: 1), .seconds(60))
    }

    func testFailuresAreClassifiedAndBackOffUntilASuccessResetsTheSchedule() async throws {
        let transport = ScriptedTransport(overviews: [.fail, .fail, .fail, .succeed], changesFail: true)
        let delays = DelayRecorder(stopAfter: 4)
        let session = SyncSession(transport: transport, identity: identity(),
            classify: { ($0 as? URLError)?.code == .notConnectedToInternet ? .noInternet : .unexpected },
            sleep: { try await delays.sleep($0) }, random: { 1 })
        var updates = await session.subscribe().makeAsyncIterator()

        let first = await updates.next()
        XCTAssertEqual(first?.connection, .connecting)
        let offline = await updates.next()
        XCTAssertEqual(offline?.connection, .offline(.noInternet))
        await delays.waitUntilFull()

        let recorded = await delays.values
        XCTAssertEqual(recorded, [.seconds(1), .seconds(2), .seconds(4), .seconds(1)])
        await session.stop()
    }

    func testRetryNowSkipsTheRemainingWait() async throws {
        let transport = ScriptedTransport(overviews: [.fail, .succeed], changesFail: false)
        let session = SyncSession(transport: transport, identity: identity(),
            sleep: { _ in try await Task.sleep(for: .seconds(3600)) })
        var updates = await session.subscribe().makeAsyncIterator()
        while await updates.next()?.connection == .connecting {}

        await session.retryNow()

        var online = false
        while let update = await updates.next() {
            if update.connection == .online { online = true; break }
        }
        XCTAssertTrue(online)
        let requests = await transport.overviewRequests
        XCTAssertEqual(requests, 2)
        await session.stop()
    }

    func testRetryNowLeavesAHealthySessionAlone() async throws {
        let transport = ScriptedTransport(overviews: [.succeed], changesFail: false)
        let session = SyncSession(transport: transport, identity: identity())
        var updates = await session.subscribe().makeAsyncIterator()
        while await updates.next()?.connection != .online {}

        await session.retryNow()
        try await Task.sleep(for: .milliseconds(50))

        let requests = await transport.overviewRequests
        XCTAssertEqual(requests, 1)
        await session.stop()
    }

    private func identity() -> SyncIdentity {
        SyncIdentity(server: URL(string: "https://relay.example")!, deviceID: "phone", credentialRevision: "one")
    }
}

private actor ScriptedTransport: SyncTransport {
    enum Step { case fail, succeed }
    private var overviews: [Step]
    private let changesFail: Bool
    private(set) var overviewRequests = 0

    init(overviews: [Step], changesFail: Bool) {
        self.overviews = overviews
        self.changesFail = changesFail
    }

    func fetchOverview() async throws -> Data {
        overviewRequests += 1
        let step = overviews.count > 1 ? overviews.removeFirst() : overviews[0]
        guard step == .succeed else { throw URLError(.notConnectedToInternet) }
        return Data(#"{"tick":1}"#.utf8)
    }

    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        if changesFail { throw URLError(.notConnectedToInternet) }
        try await Task.sleep(for: .seconds(3600))
    }
}

private actor DelayRecorder {
    private(set) var values: [Duration] = []
    private let stopAfter: Int
    private var waiter: CheckedContinuation<Void, Never>?

    init(stopAfter: Int) { self.stopAfter = stopAfter }

    func sleep(_ duration: Duration) async throws {
        values.append(duration)
        guard values.count >= stopAfter else { return }
        waiter?.resume()
        waiter = nil
        try await Task.sleep(for: .seconds(3600))
    }

    func waitUntilFull() async {
        guard values.count < stopAfter else { return }
        await withCheckedContinuation { waiter = $0 }
    }
}
