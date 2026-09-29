@testable import AlondraSync
import AlondraContracts
import Foundation
import XCTest

/// Serves sectioned reads from JSON rows; the summary and change feed can be held open.
private actor SectionServer: DomainSyncTransport {
    enum Request: Equatable {
        case summary
        case page(String, cursor: String?)
        case interaction(String)
        case request(String)
        case changes(epoch: String, after: UInt64, wait: UInt)
    }

    var epoch = "epoch"
    var revision: UInt64 = 1
    var rows: [String: [[String: Any]]] = [:]
    var tokens: [String: String] = [:]
    var truncated: [String: [String]] = [:]
    var pageSize = 100
    var interactions: [String: [String: Any]] = [:]
    var fullRequests: [String: [String: Any]] = [:]
    /// Called after a page is served, to move data between pages.
    var afterPage: (@Sendable (isolated SectionServer, String, Int) -> Void)?
    var holdSummaries = false
    var holdPages = false
    var holdInteractions = false
    private var heldReads: [CheckedContinuation<Void, Never>] = []
    private(set) var requests: [Request] = []
    private var pendingSummaries: [Int: CheckedContinuation<DomainV2.Summary, Error>] = [:]
    private var pendingChanges: [Int: CheckedContinuation<DomainV2.Changes, Error>] = [:]
    private var waiters: [(Int, CheckedContinuation<Void, Never>)] = []

    private func holdRead() async { await withCheckedContinuation { heldReads.append($0) } }

    func releaseHeldReads() {
        let held = heldReads
        heldReads = []
        held.forEach { $0.resume() }
    }

    func set(_ change: (isolated SectionServer) -> Void) { change(self) }

    func domainSummary() async throws -> DomainV2.Summary {
        let index = append(.summary)
        if holdSummaries { return try await withCheckedThrowingContinuation { pendingSummaries[index] = $0 } }
        return try summary()
    }

    func summary() throws -> DomainV2.Summary {
        let sections = Dictionary(uniqueKeysWithValues: DomainV2.Section.allCases.map { section in
            (section.rawValue, ["revision": tokens[section.rawValue] ?? "t0", "count": rows[section.rawValue]?.count ?? 0] as [String: Any])
        })
        return try decode(["protocol_version": 2, "data_epoch": epoch, "revision": revision,
                           "counts": ["roots": 0, "pending": 0, "unread_information": 0],
                           "open_tasks": 0, "connected_agents": 0, "sections": sections, "pending": []])
    }

    func releaseSummary(_ index: Int) throws {
        pendingSummaries.removeValue(forKey: index)?.resume(returning: try summary())
    }

    func domainPage<Row: Decodable & Sendable & Equatable>(
        _ section: DomainV2.Section, of row: Row.Type, cursor: String?, pendingOnly: Bool
    ) async throws -> DomainV2.Page<Row> {
        append(.page(section.rawValue, cursor: cursor))
        if holdPages { await holdRead() }
        let all = rows[section.rawValue] ?? []
        let start = cursor.flatMap(Int.init) ?? 0
        let end = min(all.count, start + pageSize)
        var page: [String: Any] = [
            "protocol_version": 2, "data_epoch": epoch, "revision": revision, "section": section.rawValue,
            "section_revision": tokens[section.rawValue] ?? "t0", "total": all.count,
            "items": Array(all[start..<end]), "truncated": truncated[section.rawValue] ?? [],
        ]
        if end < all.count { page["next_cursor"] = String(end) }
        let decoded: DomainV2.Page<Row> = try decode(page)
        afterPage?(self, section.rawValue, start)
        return decoded
    }

    func domainInteraction(_ id: DomainV2.InteractionId) async throws -> DomainV2.Item<DomainV2.Interaction> {
        append(.interaction(id.rawValue))
        if holdInteractions { await holdRead() }
        guard let row = interactions[id.rawValue] else {
            throw DomainV2.DomainError(code: "not_found", message: "gone", currentRevision: revision, subject: id.rawValue)
        }
        return try decode(["protocol_version": 2, "data_epoch": epoch, "revision": revision, "item": row, "truncated": false])
    }

    func domainRequest(_ id: DomainV2.RequestId) async throws -> DomainV2.Item<DomainV2.Request> {
        append(.request(id.rawValue))
        return try decode(["protocol_version": 2, "data_epoch": epoch, "revision": revision,
                           "item": fullRequests[id.rawValue] ?? [:], "truncated": false])
    }

    func domainChanges(epoch: DomainV2.DataEpoch, after revision: DomainV2.Revision,
                       wait seconds: UInt) async throws -> DomainV2.Changes {
        let index = append(.changes(epoch: epoch.rawValue, after: revision, wait: seconds))
        return try await withCheckedThrowingContinuation { pendingChanges[index] = $0 }
    }

    func domainRuntime() async throws -> DomainV2.Runtime {
        throw DomainV2.DomainError(code: "not_found", message: "not served", currentRevision: nil, subject: nil)
    }

    func respondChanges(_ index: Int, changed: Bool) {
        pendingChanges.removeValue(forKey: index)?.resume(returning: .init(
            protocolVersion: 2, dataEpoch: .init(rawValue: epoch), revision: revision, changed: changed, sections: tokens))
    }

    func fail(_ index: Int, code: String) {
        let error = DomainV2.DomainError(code: code, message: code, currentRevision: nil, subject: nil)
        pendingChanges.removeValue(forKey: index)?.resume(throwing: error)
        pendingSummaries.removeValue(forKey: index)?.resume(throwing: error)
    }

    @discardableResult
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

    func waitForChangesRequest() async -> Int {
        while true {
            if let index = requests.indices.last(where: { if case .changes = requests[$0] { return pendingChanges[$0] != nil } else { return false } }) {
                return index
            }
            await waitForRequests(requests.count + 1)
        }
    }

    func pages(since start: Int) -> [String] {
        requests[start...].compactMap { if case .page(let section, nil) = $0 { section } else { nil } }
    }

    private func decode<Value: Decodable>(_ object: [String: Any]) throws -> Value {
        try DomainV2.decode(Value.self, from: JSONSerialization.data(withJSONObject: object))
    }
}

private func task(_ id: String, revision: UInt64 = 1, presentation: String? = nil) -> [String: Any] {
    var row: [String: Any] = ["id": id, "request_id": "r", "title": id, "state": "running", "revision": revision,
                              "started": true, "review_prs": false, "dependencies": [], "documents": [:], "archived": false]
    if let presentation { row["final_presentation"] = presentation }
    return row
}

private func question(_ id: String, revision: UInt64, body: String = "Which one?") -> [String: Any] {
    ["attachments": [], "id": id, "scope": ["kind": "task", "task_id": "t1"], "author": ["kind": "agent", "agent_id": "a"],
     "recipient": ["kind": "user"], "body": body, "blocking": true,
     "payload": ["kind": "question", "options": []], "revision": revision, "order": revision]
}

private func inboxEntry(for interaction: String, revision: UInt64) -> [String: Any] {
    ["id": "interaction:\(interaction)", "kind": "question", "subject": ["kind": "interaction", "interaction_id": interaction],
     "title": "t1", "body": "Which one?", "task_id": "t1", "revision": revision, "order": revision, "pending": true,
     "children": [], "available_actions": []]
}

final class DomainSectionsTests: XCTestCase {
    func testOnlySectionsWhoseTokenMovedAreReadAgain() async throws {
        let server = SectionServer()
        await server.set { $0.rows["tasks"] = [task("t1")]; $0.tokens["tasks"] = "a"; $0.tokens["agents"] = "a" }
        var sections = DomainSections()
        let first = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(first.tasks.map(\.id.rawValue), ["t1"])
        let firstPages = await server.pages(since: 0)
        XCTAssertEqual(Set(firstPages), Set(DomainSections.synced.map(\.rawValue)))

        let mark = await server.requests.count
        await server.set { $0.revision = 2; $0.rows["tasks"] = [task("t1"), task("t2")]; $0.tokens["tasks"] = "b" }
        let second = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(second.tasks.map(\.id.rawValue), ["t1", "t2"])
        let secondPages = await server.pages(since: mark)
        XCTAssertEqual(secondPages, ["tasks"], "An unchanged token must not be read again")
    }

    func testStaleSectionsAreAllInFlightBeforeAnyOfThemReturns() async throws {
        let server = SectionServer()
        await server.set { $0.holdPages = true; $0.rows["tasks"] = [task("t1")] }
        let summary = try await server.summary()
        let refresh = Task {
            var sections = DomainSections()
            return try await sections.refresh(to: summary, from: server)
        }
        await server.waitForRequests(DomainSections.synced.count)
        await server.releaseHeldReads()
        let projection = try await refresh.value
        XCTAssertEqual(projection.tasks.map(\.id.rawValue), ["t1"])
    }

    func testInteractionsAreAllInFlightBeforeAnyOfThemReturns() async throws {
        let server = SectionServer()
        await server.set {
            $0.holdInteractions = true
            $0.rows["inbox"] = ["q1", "q2", "q3"].map { inboxEntry(for: $0, revision: 1) }
            for id in ["q1", "q2", "q3"] { $0.interactions[id] = question(id, revision: 1) }
        }
        let summary = try await server.summary()
        let refresh = Task {
            var sections = DomainSections()
            return try await sections.refresh(to: summary, from: server)
        }
        await server.waitForRequests(DomainSections.synced.count + 3)
        await server.releaseHeldReads()
        let projection = try await refresh.value
        XCTAssertEqual(projection.interactions.map(\.id.rawValue), ["q1", "q2", "q3"])
    }

    func testOnlyTheDemandedSectionsAreReadAndTheRestWaitForAScreen() async throws {
        let server = SectionServer()
        await server.set { $0.rows["tasks"] = [task("t1")]; $0.tokens["tasks"] = "a"; $0.tokens["agents"] = "a" }
        var sections = DomainSections()
        let first = try await sections.refresh(to: try await server.summary(), from: server, demand: [.inbox])
        XCTAssertEqual(first.tasks, [])
        let firstPages = await server.pages(since: 0)
        XCTAssertEqual(firstPages, ["inbox"])

        let mark = await server.requests.count
        let second = try await sections.refresh(to: try await server.summary(), from: server, demand: [.inbox, .tasks])
        XCTAssertEqual(second.tasks.map(\.id.rawValue), ["t1"])
        let secondPages = await server.pages(since: mark)
        XCTAssertEqual(secondPages, ["tasks"], "A section already read stays as it is")
    }

    func testEveryInteractionIsReadOnlyWhenAScreenAsksForThemAll() async throws {
        let server = SectionServer()
        await server.set {
            $0.rows["inbox"] = ["q1", "q2"].enumerated().map { inboxEntry(for: $1, revision: UInt64($0 + 1)) }
            for id in ["q1", "q2"] { $0.interactions[id] = question(id, revision: 1) }
        }
        var sections = DomainSections()
        let first = try await sections.refresh(to: try await server.summary(), from: server, demand: [.inbox])
        XCTAssertEqual(first.interactions.map(\.id.rawValue), ["q1"])
        let all = try await sections.refresh(to: try await server.summary(), from: server, demand: [.inbox, .interactions])
        XCTAssertEqual(all.interactions.map(\.id.rawValue), ["q1", "q2"])
        let reads = await server.requests.filter { $0 == .interaction("q1") }
        XCTAssertEqual(reads.count, 1, "The first card's interaction is not read again")
    }

    func testEverySectionIsPagedToTheEnd() async throws {
        let server = SectionServer()
        await server.set { $0.pageSize = 2; $0.rows["tasks"] = (1...5).map { task("t\($0)") } }
        var sections = DomainSections()
        let projection = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(projection.tasks.map(\.id.rawValue), ["t1", "t2", "t3", "t4", "t5"])
    }

    func testASectionThatMovesBetweenPagesIsReadAgainFromTheStart() async throws {
        let server = SectionServer()
        await server.set { server in
            server.pageSize = 1
            server.rows["tasks"] = [task("t1"), task("t2")]
            server.tokens["tasks"] = "a"
            server.afterPage = { server, section, start in
                guard section == "tasks", start == 0, server.tokens["tasks"] == "a" else { return }
                server.rows["tasks"] = [task("t0"), task("t1"), task("t2")]
                server.tokens["tasks"] = "b"
            }
        }
        var sections = DomainSections()
        let projection = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(projection.tasks.map(\.id.rawValue), ["t0", "t1", "t2"], "Rows from two revisions must never mix")
    }

    func testARowCutShortIsReadWhole() async throws {
        let server = SectionServer()
        let whole = String(repeating: "x", count: 40_000)
        await server.set {
            $0.rows["requests"] = [["id": "r", "body": "xx", "closed": false]]
            $0.truncated["requests"] = ["r"]
            $0.fullRequests["r"] = ["id": "r", "body": whole, "closed": false]
        }
        var sections = DomainSections()
        let projection = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(projection.requests.first?.body, whole)
    }

    func testInteractionsAreReadForInboxEntriesAndAgainOnlyWhenTheirRevisionMoves() async throws {
        let server = SectionServer()
        await server.set {
            $0.rows["inbox"] = [inboxEntry(for: "q1", revision: 3)]
            $0.tokens["inbox"] = "a"
            $0.interactions["q1"] = question("q1", revision: 3)
        }
        var sections = DomainSections()
        let first = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(first.interactions.map(\.id.rawValue), ["q1"])

        let mark = await server.requests.count
        await server.set { $0.revision = 2; $0.tokens["tasks"] = "moved" }
        _ = try await sections.refresh(to: try await server.summary(), from: server)
        let unchanged = await server.requests[mark...].filter { $0 == .interaction("q1") }
        XCTAssertTrue(unchanged.isEmpty, "An interaction whose revision did not move is not read again")

        await server.set {
            $0.revision = 3
            $0.rows["inbox"] = [inboxEntry(for: "q1", revision: 4)]
            $0.tokens["inbox"] = "b"
            $0.interactions["q1"] = question("q1", revision: 4, body: "Answered?")
        }
        let third = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(third.interactions.first?.body, "Answered?")
    }

    func testAnOpenTaskPresentationIsReadWithoutAnInboxEntry() async throws {
        let server = SectionServer()
        await server.set {
            $0.rows["tasks"] = [task("t1", presentation: "p1")]
            $0.interactions["p1"] = question("p1", revision: 1)
        }
        var sections = DomainSections()
        let projection = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(projection.interactions.map(\.id.rawValue), ["p1"])
    }

    func testRevisionCannotMoveBackwardsWithinAnEpoch() async throws {
        let server = SectionServer()
        await server.set { $0.revision = 8 }
        var sections = DomainSections()
        _ = try await sections.refresh(to: try await server.summary(), from: server)
        await server.set { $0.revision = 7 }
        do {
            _ = try await sections.refresh(to: try await server.summary(), from: server)
            XCTFail("A stale summary must not replace revision 8")
        } catch let error as DomainV2.DomainError { XCTAssertEqual(error.code, "incompatible_protocol") }
    }

    func testANewEpochReadsEverySectionAgain() async throws {
        let server = SectionServer()
        await server.set { $0.revision = 8; $0.rows["tasks"] = [task("old")] }
        var sections = DomainSections()
        _ = try await sections.refresh(to: try await server.summary(), from: server)
        let mark = await server.requests.count
        await server.set { $0.epoch = "new"; $0.revision = 1; $0.rows["tasks"] = [task("new")] }
        let projection = try await sections.refresh(to: try await server.summary(), from: server)
        XCTAssertEqual(projection.tasks.map(\.id.rawValue), ["new"])
        let pages = await server.pages(since: mark)
        XCTAssertEqual(Set(pages), Set(DomainSections.synced.map(\.rawValue)))
    }
}

final class DomainSyncSessionTests: XCTestCase {
    private func identity(_ key: String) -> SyncIdentity {
        SyncIdentity(server: URL(string: "https://relay.example.invalid")!, deviceID: "phone", credentialRevision: key)
    }

    private func nextProjection(_ iterator: inout AsyncStream<DomainSyncState>.Iterator) async -> DomainSyncState? {
        while let state = await iterator.next() {
            if state.projection != nil { return state }
        }
        return nil
    }

    func testUnchangedPollKeepsTheCursorAndAChangeReadsTheSummaryAgain() async throws {
        let server = SectionServer()
        await server.set { $0.revision = 4 }
        let session = DomainSyncSession(transport: server, identity: identity("one"))
        await session.require(DomainSyncDemand(sections: [.tasks]))
        var updates = await session.subscribe().makeAsyncIterator()
        _ = await nextProjection(&updates)
        let poll = await server.waitForChangesRequest()
        let first = await server.requests[poll]
        XCTAssertEqual(first, .changes(epoch: "epoch", after: 4, wait: 10))
        await server.respondChanges(poll, changed: false)
        await server.waitForRequests(poll + 2)
        let second = await server.requests[poll + 1]
        XCTAssertEqual(second, .changes(epoch: "epoch", after: 4, wait: 10))
        await server.set { $0.revision = 5; $0.rows["tasks"] = [task("t1")]; $0.tokens["tasks"] = "b" }
        await server.respondChanges(poll + 1, changed: true)
        let updated = await nextProjection(&updates)
        XCTAssertEqual(updated?.projection?.revision, 5)
        XCTAssertEqual(updated?.projection?.tasks.map(\.id.rawValue), ["t1"])
        let reread = await server.requests[(poll + 2)...].filter { if case .page = $0 { true } else { false } }
        XCTAssertEqual(reread, [.page("tasks", cursor: nil)])
        await session.stop()
    }

    func testFirstLoadReadsTheSummaryTheInboxAndTheFirstCardOnly() async throws {
        let server = SectionServer()
        await server.set {
            $0.rows["inbox"] = [inboxEntry(for: "q1", revision: 1), inboxEntry(for: "q2", revision: 2)]
            $0.rows["tasks"] = [task("t1")]
            for id in ["q1", "q2"] { $0.interactions[id] = question(id, revision: id == "q1" ? 1 : 2) }
        }
        let session = DomainSyncSession(transport: server, identity: identity("one"))
        var updates = await session.subscribe().makeAsyncIterator()
        let first = await nextProjection(&updates)
        let poll = await server.waitForChangesRequest()
        let reads = await server.requests[..<poll]
        XCTAssertEqual(Array(reads), [.summary, .page("inbox", cursor: nil), .interaction("q1")])
        XCTAssertEqual(first?.projection?.inbox.count, 2)
        XCTAssertEqual(first?.projection?.interactions.map(\.id.rawValue), ["q1"])
        XCTAssertEqual(first?.projection?.tasks, [])
        XCTAssertNil(first?.runtime)
        await session.stop()
    }

    func testARequiredSectionIsReadWhenAScreenAsksForIt() async throws {
        let server = SectionServer()
        await server.set { $0.rows["tasks"] = [task("t1")]; $0.tokens["tasks"] = "a" }
        let session = DomainSyncSession(transport: server, identity: identity("one"))
        var updates = await session.subscribe().makeAsyncIterator()
        _ = await nextProjection(&updates)
        let poll = await server.waitForChangesRequest()
        await session.require(DomainSyncDemand(sections: [.tasks]))
        let loaded = await nextProjection(&updates)
        XCTAssertEqual(loaded?.projection?.tasks.map(\.id.rawValue), ["t1"])
        let after = await server.requests[(poll + 1)...].filter { if case .changes = $0 { false } else { true } }
        XCTAssertEqual(after, [.summary, .page("tasks", cursor: nil)], "Only the section that was asked for is read")
        await session.stop()
    }

    func testEpochChangeClearsTheOldViewAndStartsAtTheNewRevision() async throws {
        let server = SectionServer()
        await server.set { $0.epoch = "epoch-a"; $0.revision = 7 }
        let session = DomainSyncSession(transport: server, identity: identity("one"))
        var updates = await session.subscribe().makeAsyncIterator()
        let first = await nextProjection(&updates)
        XCTAssertEqual(first?.projection?.dataEpoch.rawValue, "epoch-a")
        let poll = await server.waitForChangesRequest()
        await server.set { $0.epoch = "epoch-b"; $0.revision = 1; $0.holdSummaries = true }
        await server.fail(poll, code: "stale_epoch")
        await server.waitForRequests(poll + 2)
        let clearing = await session.state
        XCTAssertNil(clearing.projection, "A stale epoch must not remain visible as current")
        try await server.releaseSummary(poll + 1)
        let second = await nextProjection(&updates)
        XCTAssertEqual(second?.projection?.dataEpoch.rawValue, "epoch-b")
        XCTAssertEqual(second?.projection?.revision, 1)
        await session.stop()
    }

    func testLateOldIdentityResponseCannotPublishIntoNewIdentity() async throws {
        let old = SectionServer()
        let new = SectionServer()
        await old.set { $0.holdSummaries = true; $0.epoch = "old-epoch"; $0.revision = 50 }
        await new.set { $0.holdSummaries = true; $0.epoch = "new-epoch"; $0.revision = 2 }
        let session = DomainSyncSession(transport: old, identity: identity("old"))
        var updates = await session.subscribe().makeAsyncIterator()
        await old.waitForRequests(1)
        await session.use(transport: new, identity: identity("new"))
        await new.waitForRequests(1)
        try await old.releaseSummary(0)
        let pending = await session.state
        XCTAssertEqual(pending.identity, identity("new"))
        XCTAssertNil(pending.projection)
        try await new.releaseSummary(0)
        let current = await nextProjection(&updates)
        XCTAssertEqual(current?.identity, identity("new"))
        XCTAssertEqual(current?.projection?.dataEpoch.rawValue, "new-epoch")
        await session.stop()
    }

    func testIncompatibleChangeResponseClearsTheView() async throws {
        let server = SectionServer()
        let session = DomainSyncSession(transport: server, identity: identity("one"), backoff: ConnectionBackoff(initial: .seconds(60)))
        var updates = await session.subscribe().makeAsyncIterator()
        _ = await nextProjection(&updates)
        let poll = await server.waitForChangesRequest()
        await server.fail(poll, code: "incompatible_protocol")
        let failure = await updates.next()
        XCTAssertNil(failure?.projection)
        guard let failure else { return XCTFail("Expected offline state") }
        if case .offline = failure.connection {} else { XCTFail("Expected offline state") }
        await session.stop()
    }
}
