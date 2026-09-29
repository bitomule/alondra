import AlondraContracts
import AlondraDomain
import AlondraTriageFeature
import ComposableArchitecture
import Foundation
import Testing

private func action(_ kind: String, enabled: Bool = true, reason: String? = nil, revision: UInt64 = 1) -> DomainV2.AvailableAction {
    .init(kind: kind, enabled: enabled, disabledReason: reason, expectedRevision: revision, presentationId: nil, expectedHead: nil)
}

private func item(_ id: String, kind: String, order: UInt64, pending: Bool = true,
                  children: [DomainV2.InboxItem] = [], actions: [DomainV2.AvailableAction] = []) -> DomainV2.InboxItem {
    let subject: DomainV2.InboxSubject = id.hasPrefix("pr:")
        ? .pullRequest(prId: .init(rawValue: String(id.dropFirst(3))))
        : .interaction(interactionId: .init(rawValue: String(id.split(separator: ":").last!)))
    return .init(id: id, kind: kind, subject: subject, title: id, body: nil, taskId: "t", requestId: "r", agentId: "a",
                 revision: 1, order: order, pending: pending, children: children, availableActions: actions)
}

private func information(_ id: String, order: UInt64) -> DomainV2.InboxItem {
    item("interaction:\(id)", kind: "information", order: order, actions: [action("mark_read")])
}

private func workspace(_ roots: [DomainV2.InboxItem], revision: UInt64 = 1, epoch: String = "epoch") -> DomainWorkspace {
    DomainWorkspace(.init(protocolVersion: 2, dataEpoch: .init(rawValue: epoch), revision: revision, inbox: roots,
                          counts: .init(roots: UInt64(roots.count), pending: 0, unreadInformation: 0), tasks: [], agents: [],
                          requests: [], interactions: [], pullRequests: [], documents: [], documentRevisions: [],
                          documentReviews: [], mergeAuthorizations: [], stackReadiness: []))
}

private actor Recorder {
    var intents: [DomainIntent] = []
    var commandIDs: [String] = []
    func record(_ intent: DomainIntent, _ commandID: String) { intents.append(intent); commandIDs.append(commandID) }
}

@MainActor
struct TriageFeatureTests {
    private let a = TriageIdentity(server: URL(string: "https://a.example")!, deviceID: "phone", credentialRevision: "key-a", generation: 1)
    private let b = TriageIdentity(server: URL(string: "https://b.example")!, deviceID: "phone", credentialRevision: "key-b", generation: 2)

    private func client(_ recorder: Recorder = Recorder(), result: DomainCommandResult = .obsolete,
                        updates: AsyncStream<DomainWorkspaceUpdate> = AsyncStream { $0.finish() }) -> DomainWorkspaceClient {
        var client = DomainWorkspaceClient.inert
        client.observe = { updates }
        client.perform = { intent, _, _, commandID in
            await recorder.record(intent, commandID)
            return result
        }
        return client
    }

    @Test
    func queueHoldsPendingServerRootsInServerOrderAndChildrenStayInside() async throws {
        let question = item("interaction:q", kind: "question", order: 5, actions: [action("answer_question")])
        let answered = item("interaction:old", kind: "question", order: 1, pending: false)
        let pr = item("pr:pr", kind: "pull_request", order: 3, children: [question])
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.domainWorkspace = client()
        }
        store.exhaustivity = .off
        await store.send(.workspaceUpdated(.init(identity: a, workspace: workspace([information("info", order: 7), pr, answered]),
                                                 connection: .online)))
        #expect(store.state.queue.map(\.id) == ["pr:pr", "interaction:info"])
        #expect(store.state.routeTarget(for: "interaction:q")?.id == "pr:pr")
        await store.send(.route("interaction:q", opensDetail: true)) {
            $0.presentedID = "pr:pr"
        }
    }

    @Test
    func staleIdentityCannotChangeConnectionOrQueue() async throws {
        let updates = AsyncStream<DomainWorkspaceUpdate>.makeStream()
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.domainWorkspace = client(updates: updates.stream)
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        updates.continuation.yield(.init(identity: a, workspace: workspace([information("info", order: 1)]), connection: .online))
        await store.receive(\.workspaceUpdated)
        updates.continuation.yield(.init(identity: b, workspace: workspace([]), connection: .offline(.macNotAnswering)))
        await store.receive(\.workspaceUpdated)
        #expect(store.state.connection == .online)
        #expect(store.state.queue.map(\.id) == ["interaction:info"])
        await store.send(.disappeared)
        updates.continuation.finish()
        await store.finish()
    }

    @Test
    func readingInformationSendsOnlyMarkReadForThatInteraction() async throws {
        let recorder = Recorder()
        var initial = TriageFeature.State()
        initial.identity = a
        let current = workspace([information("info", order: 1), information("other", order: 2)])
        initial.workspace = current
        initial.items = current.roots
        let store = TestStore(initialState: initial) { TriageFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: .recorded(.init(revision: 2, changed: true, effects: [], createdIds: [])))
            $0.uuid = .constant(UUID(0))
        }
        await store.send(.readWithPresentation("interaction:info")) {
            $0.pendingIDs = ["interaction:info"]
            $0.deferredReadIDs = ["interaction:info"]
            $0.commandIDs = ["interaction:info": UUID(0).uuidString.lowercased()]
        }
        await store.receive(\.mutationFinished) { $0.pendingIDs = []; $0.commandIDs = [:] }
        await store.receive(\.readAcknowledged)
        #expect(await recorder.intents == [.markRead("info")])
        await store.send(.workspaceUpdated(.init(identity: a, workspace: workspace([information("other", order: 2)], revision: 2),
                                                 connection: .online))) {
            $0.workspace = workspace([information("other", order: 2)], revision: 2)
            $0.connection = .online
            $0.hasSnapshot = true
            $0.snapshotRevision = 1
            $0.items = [information("info", order: 1), information("other", order: 2)]
        }
        await store.send(.readPresentationFinished("interaction:info")) {
            $0.deferredReadIDs = []
            $0.hiddenIDs = ["interaction:info"]
            $0.resolvedCount = 1
        }
        #expect(store.state.queue.map(\.id) == ["interaction:other"])
    }

    @Test
    func onlyAnUnreadNoticeLeavesTheInboxByReading() async throws {
        let recorder = Recorder()
        var initial = TriageFeature.State()
        initial.identity = a
        let current = workspace([item("interaction:q", kind: "question", order: 1, actions: [action("answer_question")]),
                                 item("pr:pr", kind: "pull_request", order: 2),
                                 item("interaction:done", kind: "information", order: 3,
                                      actions: [action("mark_read", enabled: false, reason: "already_read")])])
        initial.workspace = current
        initial.items = current.roots
        let store = TestStore(initialState: initial) { TriageFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.hide("interaction:q"))
        await store.send(.readWithPresentation("interaction:q"))
        await store.send(.readWithPresentation("pr:pr"))
        await store.send(.hide("interaction:done"))
        await store.send(.hide("pr:pr"))
        #expect(await recorder.intents.isEmpty, "A question, a pull request and a read notice stay until decided")
        #expect(store.state.hiddenIDs.isEmpty)
    }

    @Test
    func rejectionRestoresTheHiddenCardAndShowsTheServerMessage() async throws {
        var initial = TriageFeature.State()
        initial.identity = a
        let current = workspace([information("info", order: 1)])
        initial.workspace = current
        initial.items = current.roots
        let store = TestStore(initialState: initial) { TriageFeature() } withDependencies: {
            $0.domainWorkspace = client(result: .rejected(.init(code: "not_found", message: "Refresh first")))
            $0.uuid = .constant(UUID(0))
        }
        await store.send(.hide("interaction:info")) {
            $0.cursor = current.roots[0].place
            $0.hiddenIDs = ["interaction:info"]
            $0.pendingIDs = ["interaction:info"]
            $0.commandIDs = ["interaction:info": UUID(0).uuidString.lowercased()]
        }
        await store.receive(\.mutationFinished) {
            $0.pendingIDs = []
            $0.commandIDs = [:]
            $0.hiddenIDs = []
            $0.noticeIsMutationFailure = true
            $0.notice = "This item is no longer available."
        }
    }

    @Test
    func anUnknownOutcomeKeepsTheCardAndTryingAgainReusesTheCommandID() async throws {
        let recorder = Recorder()
        var initial = TriageFeature.State()
        initial.identity = a
        let current = workspace([information("info", order: 1)])
        initial.workspace = current
        initial.items = current.roots
        let store = TestStore(initialState: initial) { TriageFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: .uncertain)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.hide("interaction:info"))
        await store.receive(\.mutationFinished)
        #expect(store.state.queue.map(\.id) == ["interaction:info"])
        #expect(store.state.notice != nil)
        await store.send(.hide("interaction:info"))
        await store.receive(\.mutationFinished)
        let ids = await recorder.commandIDs
        #expect(ids.count == 2)
        #expect(Set(ids).count == 1, "A retry must reuse the command ID so the server applies it once")
    }

    @Test
    func identitySwitchClearsTheQueueAndIgnoresLateResults() async throws {
        var initial = TriageFeature.State()
        initial.identity = a
        let current = workspace([information("info", order: 1)])
        initial.workspace = current
        initial.items = current.roots
        initial.pendingIDs = ["interaction:info"]
        let store = TestStore(initialState: initial) { TriageFeature() } withDependencies: {
            $0.domainWorkspace = client()
        }
        store.exhaustivity = .off
        await store.send(.identityChanged(b))
        #expect(store.state.items.isEmpty)
        #expect(store.state.workspace == nil)
        await store.send(.mutationFinished("interaction:info", a, .confirmed))
        #expect(store.state.resolvedCount == 0)
    }
}

struct InboxRingTests {
    @Test
    func cursorFollowsServerOrderAndSurvivesTheFrontLeaving() {
        let items = [information("a", order: 1), information("b", order: 2), information("c", order: 3)]
        var ring = InboxRing()
        ring.move(from: items[0], by: 1, in: items)
        #expect(ring.arranged(items).map(\.id) == ["interaction:b", "interaction:c", "interaction:a"])
        ring.resolve(items[1], in: items)
        let remaining = [items[0], items[2]]
        #expect(ring.arranged(remaining).map(\.id) == ["interaction:c", "interaction:a"])
    }
}
