import AlondraContracts
import AlondraSync
import Foundation
import XCTest

private actor ControlledDomainTransport: DomainSyncTransport {
    enum Request: Equatable {
        case projection
        case changes(epoch: String, after: UInt64, wait: UInt)
    }

    private var requests: [Request] = []
    private var pendingProjections: [Int: CheckedContinuation<DomainV2.DomainProjection, Error>] = [:]
    private var pendingChanges: [Int: CheckedContinuation<DomainV2.Changes, Error>] = [:]
    private var waiters: [(Int, CheckedContinuation<Void, Never>)] = []

    func domainProjection() async throws -> DomainV2.DomainProjection {
        let index = append(.projection)
        return try await withCheckedThrowingContinuation { pendingProjections[index] = $0 }
    }

    func domainChanges(epoch: DomainV2.DataEpoch, after revision: DomainV2.Revision,
                       wait seconds: UInt) async throws -> DomainV2.Changes {
        let index = append(.changes(epoch: epoch.rawValue, after: revision, wait: seconds))
        return try await withCheckedThrowingContinuation { pendingChanges[index] = $0 }
    }

    private func append(_ request: Request) -> Int {
        let index = requests.count
        requests.append(request)
        let ready = waiters.filter { requests.count >= $0.0 }
        waiters.removeAll { requests.count >= $0.0 }
        for (_, waiter) in ready { waiter.resume() }
        return index
    }

    func waitForRequests(_ count: Int) async {
        if requests.count >= count { return }
        await withCheckedContinuation { waiters.append((count, $0)) }
    }

    func request(_ index: Int) -> Request { requests[index] }

    func respondProjection(_ index: Int, epoch: String, revision: UInt64) throws {
        let json = """
        {"protocol_version":2,"data_epoch":"\(epoch)","revision":\(revision),
         "inbox":[],"counts":{"roots":0,"pending":0,"unread_information":0},
         "tasks":[],"agents":[],"requests":[],"interactions":[],"pull_requests":[],
         "documents":[],"document_revisions":[],"document_reviews":[],
         "merge_authorizations":[],"stack_readiness":[]}
        """
        let projection = try DomainV2.decode(DomainV2.DomainProjection.self, from: Data(json.utf8))
        pendingProjections.removeValue(forKey: index)?.resume(returning: projection)
    }

    func respondChanges(_ index: Int, epoch: String, revision: UInt64, changed: Bool) {
        pendingChanges.removeValue(forKey: index)?.resume(returning: .init(
            protocolVersion: 2, dataEpoch: .init(rawValue: epoch), revision: revision, changed: changed))
    }

    func staleEpoch(_ index: Int) {
        pendingChanges.removeValue(forKey: index)?.resume(throwing: DomainV2.DomainError(
            code: "stale_epoch", message: "Epoch changed", currentRevision: nil, subject: nil))
    }

    func incompatibleResponse(_ index: Int) {
        pendingChanges.removeValue(forKey: index)?.resume(throwing: DomainV2.DomainError(
            code: "incompatible_protocol", message: "Invalid changes", currentRevision: nil, subject: nil))
    }
}

final class DomainSyncSessionTests: XCTestCase {
    private func identity(_ key: String) -> SyncIdentity {
        SyncIdentity(server: URL(string: "https://relay.example.invalid")!,
                     deviceID: "phone", credentialRevision: key)
    }

    private func nextProjection(_ iterator: inout AsyncStream<DomainSyncState>.Iterator) async -> DomainSyncState? {
        while let state = await iterator.next() {
            if state.projection != nil { return state }
        }
        return nil
    }

    func testEpochChangeClearsOldProjectionAndStartsAtNewRevision() async throws {
        let transport = ControlledDomainTransport()
        let session = DomainSyncSession(transport: transport, identity: identity("one"))
        var updates = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        try await transport.respondProjection(0, epoch: "epoch-a", revision: 7)
        let firstResult = await nextProjection(&updates)
        let first = try XCTUnwrap(firstResult)
        XCTAssertEqual(first.projection?.dataEpoch.rawValue, "epoch-a")
        await transport.waitForRequests(2)
        let request = await transport.request(1)
        XCTAssertEqual(request, .changes(epoch: "epoch-a", after: 7, wait: 10))
        await transport.staleEpoch(1)
        await transport.waitForRequests(3)
        let clearing = await session.state
        XCTAssertNil(clearing.projection, "Stale epoch must not remain visible as current")
        try await transport.respondProjection(2, epoch: "epoch-b", revision: 1)
        let secondResult = await nextProjection(&updates)
        let second = try XCTUnwrap(secondResult)
        XCTAssertEqual(second.projection?.dataEpoch.rawValue, "epoch-b")
        XCTAssertEqual(second.projection?.revision, 1)
        await session.stop()
    }

    func testLateOldIdentityResponseCannotPublishIntoNewIdentity() async throws {
        let old = ControlledDomainTransport()
        let new = ControlledDomainTransport()
        let session = DomainSyncSession(transport: old, identity: identity("old"))
        var updates = await session.subscribe().makeAsyncIterator()
        await old.waitForRequests(1)
        await session.use(transport: new, identity: identity("new"))
        await new.waitForRequests(1)
        try await old.respondProjection(0, epoch: "old-epoch", revision: 50)
        let pending = await session.state
        XCTAssertEqual(pending.identity, identity("new"))
        XCTAssertNil(pending.projection)
        try await new.respondProjection(0, epoch: "new-epoch", revision: 2)
        let currentResult = await nextProjection(&updates)
        let current = try XCTUnwrap(currentResult)
        XCTAssertEqual(current.identity, identity("new"))
        XCTAssertEqual(current.projection?.dataEpoch.rawValue, "new-epoch")
        await session.stop()
    }

    func testUnchangedPollKeepsCursorAndChangedPollFetchesFullProjection() async throws {
        let transport = ControlledDomainTransport()
        let session = DomainSyncSession(transport: transport, identity: identity("one"))
        var updates = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        try await transport.respondProjection(0, epoch: "epoch", revision: 4)
        _ = await nextProjection(&updates)
        await transport.waitForRequests(2)
        await transport.respondChanges(1, epoch: "epoch", revision: 4, changed: false)
        await transport.waitForRequests(3)
        let secondPoll = await transport.request(2)
        XCTAssertEqual(secondPoll, .changes(epoch: "epoch", after: 4, wait: 10))
        await transport.respondChanges(2, epoch: "epoch", revision: 5, changed: true)
        await transport.waitForRequests(4)
        let refreshRequest = await transport.request(3)
        XCTAssertEqual(refreshRequest, .projection)
        try await transport.respondProjection(3, epoch: "epoch", revision: 5)
        let updatedResult = await nextProjection(&updates)
        let updated = try XCTUnwrap(updatedResult)
        XCTAssertEqual(updated.projection?.revision, 5)
        await session.stop()
    }

    func testIncompatibleChangeResponseClearsProjection() async throws {
        let transport = ControlledDomainTransport()
        let session = DomainSyncSession(transport: transport, identity: identity("one"), backoff: ConnectionBackoff(initial: .seconds(60)))
        var updates = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        try await transport.respondProjection(0, epoch: "epoch", revision: 4)
        _ = await nextProjection(&updates)
        await transport.waitForRequests(2)
        await transport.incompatibleResponse(1)
        let failure = await updates.next()
        XCTAssertNil(failure?.projection)
        guard let failure else { return XCTFail("Expected offline state") }
        if case .offline(_) = failure.connection {} else { XCTFail("Expected offline state") }
        await session.stop()
    }

    func testRefetchCannotPublishLowerRevisionWithinSameEpoch() async throws {
        let transport = ControlledDomainTransport()
        let session = DomainSyncSession(transport: transport, identity: identity("one"), backoff: ConnectionBackoff(initial: .seconds(60)))
        var updates = await session.subscribe().makeAsyncIterator()
        await transport.waitForRequests(1)
        try await transport.respondProjection(0, epoch: "epoch", revision: 8)
        _ = await nextProjection(&updates)
        await transport.waitForRequests(2)
        await transport.respondChanges(1, epoch: "epoch", revision: 9, changed: true)
        await transport.waitForRequests(3)
        try await transport.respondProjection(2, epoch: "epoch", revision: 7)
        let rejected = await updates.next()
        XCTAssertNil(rejected?.projection, "A stale 200 snapshot must not replace revision 8")
        guard let rejected else { return XCTFail("Expected rejected state") }
        if case .offline(_) = rejected.connection {} else { XCTFail("Expected protocol error") }
        await session.stop()
    }
}
