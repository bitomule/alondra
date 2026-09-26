import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct TriageFeatureTests {
    private let a = TriageIdentity(server: URL(string: "https://a.example")!, deviceID: "phone", credentialRevision: "key-a", generation: 1)
    private let b = TriageIdentity(server: URL(string: "https://b.example")!, deviceID: "phone", credentialRevision: "key-b", generation: 2)

    private func overview(_ questions: String, reports: String = "", prs: String = "") throws -> Overview {
        let json = #"{"tick":1,"nodes":[],"open_questions":[\#(questions)],"news":[\#(reports)],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false},"prs":[\#(prs)]}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Overview.self, from: Data(json.utf8))
    }

    private func question(_ id: Int, at: Int = 1, pr: Int? = nil) -> String {
        let linked = pr.map { ",\"prId\":\($0)" } ?? ""
        return #"{"id":\#(id),"sessionId":"s","title":"Q\#(id)","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":\#(at)\#(linked)}"#
    }

    private func report(_ id: Int, pr: Int? = nil) -> String {
        let linked = pr.map { ",\"prId\":\($0)" } ?? ""
        return #"{"id":\#(id),"sessionId":"s","title":"R\#(id)","links":[],"attachments":[],"kind":"final","createdAt":2\#(linked)}"#
    }

    @Test
    func oneCardPerIDAndAttachedQuestionStaysInsidePR() throws {
        let pr = #"{"id":9,"url":"https://example.test/p/9","repo":"a/b","number":9,"sessionId":"s","title":"PR","state":"open","openedAt":3}"#
        let snapshot = try overview(
            [question(2, at: 20), question(1, at: 10), question(1, at: 10), question(3, pr: 9), question(3, pr: 9)].joined(separator: ","),
            reports: [report(4), report(5, pr: 9), report(5, pr: 9)].joined(separator: ","), prs: pr
        )
        let inbox = ObserveInboxUseCase().execute(snapshot)
        #expect(inbox.items.map(\.id) == ["q1", "q2", "p9", "r4"])
        #expect(inbox.questionsByPullRequest[9]?.map(\.id) == [3])
        #expect(inbox.reportsByPullRequest[9]?.map(\.id) == [5])
    }

    @Test
    func pushForGroupedQuestionRoutesToItsVisiblePRAndOldIdentityCannotReopenIt() async throws {
        let pr = #"{"id":9,"url":"https://example.test/p/9","repo":"a/b","number":9,"sessionId":"s","title":"PR","state":"open","openedAt":3}"#
        let snapshot = try overview(question(3, pr: 9), prs: pr)
        let inbox = ObserveInboxUseCase().execute(snapshot)
        let expectedIdentity = a
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.triageService = TriageService(
                observe: { AsyncStream { $0.finish() } },
                mutate: { kind, itemID, identity in
                    kind == .dismissPullRequest(9) && itemID == "p9" && identity == expectedIdentity
                        ? .confirmed : .rejected("Wrong command scope")
                }
            )
        }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: snapshot))) {
            $0.snapshotRevision &+= 1
            $0.items = inbox.items
            $0.questionsByPullRequest = inbox.questionsByPullRequest
            $0.hasSnapshot = true
        }
        #expect(store.state.routeTarget(for: "q3")?.id == "p9")
        await store.send(.route("q3", opensDetail: true)) {
            $0.cursor = inbox.items[0].place
            $0.presentedID = "p9"
        }
        await store.send(.hide("p9")) {
            $0.hiddenIDs = ["p9"]
            $0.pendingIDs = ["p9"]
            $0.presentedID = nil
        }
        await store.receive(\.mutationFinished) {
            $0.pendingIDs = []
            $0.resolvedCount = 1
        }
        await store.send(.identityChanged(b)) {
            $0.identity = b
            $0.items = []
            $0.questionsByPullRequest = [:]
            $0.hiddenIDs = []
            $0.cursor = nil
            $0.presentedID = nil
            $0.hasSnapshot = false
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: snapshot)))
        await store.send(.route("q3", opensDetail: true))
        #expect(store.state.presentedID == nil)
    }

    @Test
    func pushWaitsForLaterOverviewOpensNewGroupedChildAndSettlesMissingRouteAfterIdenticalFetch() async throws {
        let pr = #"{"id":9,"url":"https://example.test/p/9","repo":"a/b","number":9,"sessionId":"s","title":"PR","state":"open","openedAt":3}"#
        let before = try overview("", prs: pr)
        let after = try overview(question(3, pr: 9), prs: pr)
        let beforeInbox = ObserveInboxUseCase().execute(before)
        let afterInbox = ObserveInboxUseCase().execute(after)
        #expect(beforeInbox.items == afterInbox.items)
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: before))) {
            $0.items = beforeInbox.items
            $0.hasSnapshot = true
            $0.snapshotRevision = 1
        }
        let waiting = TriageRouteWait(itemID: "q3", afterSnapshotRevision: store.state.snapshotRevision)
        #expect(waiting.resolve(snapshotRevision: store.state.snapshotRevision, targets: store.state.routeTargets) == .waiting)
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: after))) {
            $0.questionsByPullRequest = afterInbox.questionsByPullRequest
            $0.snapshotRevision = 2
        }
        #expect(waiting.resolve(snapshotRevision: store.state.snapshotRevision, targets: store.state.routeTargets) == .open("p9"))
        let missing = TriageRouteWait(itemID: "q99", afterSnapshotRevision: store.state.snapshotRevision)
        #expect(missing.resolve(snapshotRevision: store.state.snapshotRevision, targets: store.state.routeTargets) == .waiting)
        // The session fetched the same overview again: no item changed, but this is a fresh
        // accepted projection and an absent push target can now be declared unavailable.
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: after))) {
            $0.snapshotRevision = 3
        }
        #expect(missing.resolve(snapshotRevision: store.state.snapshotRevision, targets: store.state.routeTargets) == .unavailable)
    }

    @Test
    func delayedSkipCannotMoveSameCardIDAfterCredentialGenerationChanges() async throws {
        let snapshot = try overview(question(1) + "," + question(2, at: 2))
        let inbox = ObserveInboxUseCase().execute(snapshot)
        let rotated = TriageIdentity(server: a.server, deviceID: a.deviceID,
            credentialRevision: a.credentialRevision, generation: a.generation + 1)
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: snapshot))) {
            $0.items = inbox.items
            $0.hasSnapshot = true
            $0.snapshotRevision = 1
        }
        await store.send(.identityChanged(rotated)) {
            $0.identity = rotated
            $0.items = []
            $0.hasSnapshot = false
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: rotated, overview: snapshot))) {
            $0.items = inbox.items
            $0.hasSnapshot = true
            $0.snapshotRevision = 2
        }
        await store.send(.skipIfCurrent("q1", a))
        #expect(store.state.queue.map(\.id) == ["q1", "q2"])
        await store.send(.skipIfCurrent("q1", rotated)) {
            $0.cursor = inbox.items[1].place
            $0.skippedCount = 1
        }
    }

    @Test
    func rejectionRestoresHiddenFrontCardAndShowsReason() async throws {
        let snapshot = try overview(question(1) + "," + question(2, at: 2), reports: report(4))
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.triageService = TriageService(
                observe: { AsyncStream { $0.finish() } },
                mutate: { _, _, _ in .rejected("Server refused") }
            )
        }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: snapshot))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(snapshot).items
            $0.hasSnapshot = true
        }
        await store.send(.skip("q1")) {
            $0.cursor = $0.items[1].place
            $0.skippedCount = 1
        }
        await store.send(.focus("r4")) { $0.cursor = $0.items[2].place }
        await store.send(.hide("r4")) {
            $0.hiddenIDs = ["r4"]
            $0.pendingIDs = ["r4"]
        }
        await store.receive(\.mutationFinished) {
            $0.hiddenIDs = []
            $0.pendingIDs = []
            $0.notice = "Server refused"
        }
        #expect(store.state.queue.map(\.id) == ["r4", "q1", "q2"])
    }

    @Test
    func identitySwitchClearsQueueAndIgnoresLateMutationAndSnapshot() async throws {
        let snapshot = try overview("", reports: report(4))
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.triageService = TriageService(
                observe: { AsyncStream { $0.finish() } },
                mutate: { _, _, _ in .confirmed }
            )
        }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: snapshot))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(snapshot).items
            $0.hasSnapshot = true
        }
        await store.send(.review("r4")) { $0.presentedID = "r4" }
        await store.send(.identityChanged(b)) {
            $0.identity = b
            $0.items = []
            $0.presentedID = nil
            $0.hasSnapshot = false
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: snapshot)))
        await store.send(.mutationFinished("r4", a, .confirmed))
        #expect(store.state.queue.isEmpty)
        #expect(store.state.presentedID == nil)
    }

    @Test
    func withdrawnCardDisappearsAndClosesItsDetail() async throws {
        let first = try overview(question(1) + "," + question(2, at: 2))
        let withdrawn = try overview(question(2, at: 2))
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: first))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(first).items
            $0.hasSnapshot = true
        }
        await store.send(.review("q1")) { $0.presentedID = "q1" }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: withdrawn))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(withdrawn).items
            $0.presentedID = nil
        }
        #expect(store.state.queue.map(\.id) == ["q2"])
    }

    @Test
    func confirmedHideStaysHiddenUntilServerWithdrawsCard() async throws {
        let stale = try overview(question(1), reports: report(4))
        let resolved = try overview(question(1))
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.triageService = TriageService(
                observe: { AsyncStream { $0.finish() } },
                mutate: { _, _, _ in .confirmed }
            )
        }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: stale))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(stale).items
            $0.hasSnapshot = true
        }
        await store.send(.hide("r4")) {
            $0.hiddenIDs = ["r4"]
            $0.pendingIDs = ["r4"]
        }
        await store.receive(\.mutationFinished) {
            $0.pendingIDs = []
            $0.resolvedCount = 1
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: stale))) {
            $0.snapshotRevision &+= 1
        }
        #expect(store.state.queue.map(\.id) == ["q1"])
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: resolved))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(resolved).items
            $0.hiddenIDs = []
        }
    }

    @Test
    func disappearingAndReappearingSnapshotCannotSubmitSameInFlightHideTwice() async throws {
        let stale = try overview("", reports: report(4))
        let omitted = try overview("")
        let (results, continuation) = AsyncStream<TriageMutationOutcome>.makeStream()
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.triageService = TriageService(
                observe: { AsyncStream { $0.finish() } },
                mutate: { _, _, _ in
                    for await result in results { return result }
                    return .obsolete
                }
            )
        }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: stale))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(stale).items
            $0.hasSnapshot = true
        }
        await store.send(.hide("r4")) {
            $0.hiddenIDs = ["r4"]
            $0.pendingIDs = ["r4"]
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: omitted))) {
            $0.snapshotRevision &+= 1
            $0.items = []
            $0.hiddenIDs = []
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: stale))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(stale).items
        }
        await store.send(.hide("r4"))
        continuation.yield(.confirmed)
        continuation.finish()
        await store.receive(\.mutationFinished) {
            $0.hiddenIDs = ["r4"]
            $0.pendingIDs = []
            $0.resolvedCount = 1
        }
        #expect(store.state.queue.isEmpty)
    }

    @Test
    func confirmationDuringOmissionStillSuppressesLaterStaleReappearance() async throws {
        let stale = try overview("", reports: report(4))
        let omitted = try overview("")
        let (results, continuation) = AsyncStream<TriageMutationOutcome>.makeStream()
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.triageService = TriageService(
                observe: { AsyncStream { $0.finish() } },
                mutate: { _, _, _ in
                    for await result in results { return result }
                    return .obsolete
                }
            )
        }
        await store.send(.identityChanged(a)) { $0.identity = a }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: stale))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(stale).items
            $0.hasSnapshot = true
        }
        await store.send(.hide("r4")) {
            $0.hiddenIDs = ["r4"]
            $0.pendingIDs = ["r4"]
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: omitted))) {
            $0.snapshotRevision &+= 1
            $0.items = []
            $0.hiddenIDs = []
        }
        continuation.yield(.confirmed)
        continuation.finish()
        await store.receive(\.mutationFinished) {
            $0.hiddenIDs = ["r4"]
            $0.pendingIDs = []
            $0.resolvedCount = 1
        }
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: stale))) {
            $0.snapshotRevision &+= 1
            $0.items = ObserveInboxUseCase().execute(stale).items
        }
        #expect(store.state.queue.isEmpty)
        await store.send(.inboxUpdated(TriageInboxUpdate(identity: a, overview: omitted))) {
            $0.snapshotRevision &+= 1
            $0.items = []
            $0.hiddenIDs = []
        }
    }
}
