import AlondraAttachmentsFeature
import AlondraDomain
import AlondraFleetFeature
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct StableDetailOwnerIntegrationTests {
    private func twoInformationItems() -> (V2Fixture, String, String) {
        var fixture = V2Fixture()
        let a = fixture.information("info-1", title: "Report 1")
        let b = fixture.information("info-2", title: "Report 2")
        return (fixture, a.id, b.id)
    }

    private static let receipt = DomainV2.AttachmentRef(id: "receipt", name: "proof.txt", contentDigest: Data("proof".utf8).sha256Hex,
                                                        contentReference: "ref", bytes: 5)

    @Test(.timeLimit(.minutes(1)))
    func navigatingDuringFinalKeepsItsOwnerAndOtherDraft() async throws {
        let (fixture, a, b) = twoInformationItems()
        let log = CommandLog()
        let gate = CommandGate()
        let store = Store(initialState: ServiceTriageFeature.State()) { ServiceTriageFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in await gate.run() }
        }
        await store.send(.triage(.workspaceUpdated(fixture.update()))).finish()
        await store.send(.open(a)).finish()
        await store.send(.documents(.element(id: a, action: .appeared))).finish()
        await store.send(.documents(.element(id: a, action: .generalChanged("A draft")))).finish()
        await store.send(.open(b)).finish()
        await store.send(.documents(.element(id: b, action: .generalChanged("B draft")))).finish()
        await store.send(.open(a)).finish()
        #expect(store.document?.draft.general == "A draft")
        let submission = store.send(.documents(.element(id: a, action: .sendFeedbackTapped(author: ""))))
        await gate.waitUntilStarted()
        await store.send(.open(b)).finish()
        #expect(store.documents[id: a]?.isSubmitting == true)
        #expect(store.document?.draft.general == "B draft")
        // Reselecting A must not create a fresh idle owner that permits a second send.
        await store.send(.open(a)).finish()
        await store.send(.documents(.element(id: a, action: .sendFeedbackTapped(author: "")))).finish()
        await store.send(.open(b)).finish()
        await gate.release(V2Fixture.recorded)
        await submission.finish()
        #expect(log.intents == [.replyToInformation("info-1", body: "A draft", attachments: [])])
        #expect(store.documents[id: a] == nil)
        #expect(store.triage.hiddenIDs.contains(a))
        #expect(store.documentID == b)
        #expect(store.document?.draft.general == "B draft")
        #expect(store.document?.completed == false)
    }

    @Test
    func leavingPendingUploadRetainsReceiptWithoutSendingAndIdentityClearsOwners() async throws {
        let (fixture, a, b) = twoInformationItems()
        let workspace = fixture.workspace
        let subject = try #require(workspace.item(a).flatMap { DocumentSubject(item: $0, in: workspace) })
        var owner = DocumentReviewFeature.State(subject: subject, workspace: workspace)
        let file = AttachmentDraft(name: "proof.txt", data: Data("proof".utf8))
        owner.draft.general = "Keep me"
        owner.sourceText = "Body"
        owner.attachmentPicker.entries = [.init(draft: file, status: .uploading)]
        owner.pendingReview = .reply("Keep me")
        var initial = ServiceTriageFeature.State()
        initial.document = owner
        let log = CommandLog()
        let store = Store(initialState: initial) { ServiceTriageFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in V2Fixture.recorded }
        }
        await store.send(.triage(.workspaceUpdated(fixture.update()))).finish()
        await store.send(.open(b)).finish()
        await store.send(.documents(.element(id: a, action: .attachmentPicker(.uploaded(file.id, .success(Self.receipt)))))).finish()
        await store.send(.open(a)).finish()
        #expect(store.document?.draft.general == "Keep me")
        #expect(store.document?.attachmentPicker.stored == [Self.receipt])
        #expect(store.document?.pendingReview == nil)
        #expect(log.count == 0)
        await store.send(.triage(.identityChanged(V2Fixture.other))).finish()
        #expect(store.documents.isEmpty)
        #expect(store.documentID == nil)
    }

    @Test(.timeLimit(.minutes(1)), arguments: [false, true])
    func nodeFinalAndLateRetryRemainWithOriginalNode(stackNavigation: Bool) async throws {
        func node(_ id: String) throws -> Node {
            try JSONDecoder().decode(Node.self, from: Data("""
            {"sessionId":"\(id)","shortId":"\(id)","name":"\(id)","project":"Alondra","state":"working","stateSince":1,"isOrchestrator":false,"hasPlugin":true,"stateSource":"hook"}
            """.utf8))
        }
        let a = try node("a"), b = try node("b")
        var initial = ServiceRootFeature.State()
        initial.fleet.fleet.snapshot = FleetSnapshot(sequence: 1, tick: 1, orchestrator: nil,
            sections: [.init(name: "Alondra", nodes: [.init(node: a, tasks: [], pendingCount: 0), .init(node: b, tasks: [], pendingCount: 0)])])
        let gate = CommandGate()
        let sent = LockIsolated<[(String, String)]>([])
        let opened = AsyncStream<String>.makeStream()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .fleet(.nodes(.element(let id, .snapshotChanged))) = action {
                        opened.continuation.yield(id)
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.uuid = .incrementing
            $0.nodeOperations = .init(send: { id, _, command in
                sent.withValue { $0.append((id, command)) }
                return switch await gate.run() {
                case .recorded: NodeMutationOutcome.confirmed
                case .uncertain: .unknown
                default: .obsolete
                }
            }, close: { _, _ in .rejected("Unexpected close") }, threads: { _ in [] })
        }
        // Stack presentation effects live until pop. Await the snapshot action that makes each
        // destination usable, then join its lifetime only after unmounting.
        var navigationTasks: [StoreTask] = []
        var nextPathID = 0
        func open(_ id: String) async {
            if stackNavigation {
                if let first = store.fleet.path.ids.first {
                    await store.send(.fleet(.path(.popFrom(id: first)))).finish()
                }
                let elementID = StackElementID(integerLiteral: nextPathID)
                nextPathID += 1
                navigationTasks.append(store.send(.fleet(.path(.push(id: elementID, state: .node(.init(sessionID: id)))))))
                var snapshots = opened.stream.makeAsyncIterator()
                #expect(await snapshots.next() == id)
            } else {
                await store.send(.fleet(.openNode(id))).finish()
            }
        }
        await open("a")
        await store.send(.fleet(.nodes(.element(id: "a", action: .messageChanged("A message"))))).finish()
        await open("b")
        await store.send(.fleet(.nodes(.element(id: "b", action: .messageChanged("B message"))))).finish()
        await open("a")
        #expect(store.fleet.detail?.message == "A message")
        let submission = store.send(.fleet(.nodes(.element(id: "a", action: .sendTapped))))
        await gate.waitUntilStarted()
        await open("b")
        await gate.release(.uncertain)
        await submission.finish()
        #expect(store.fleet.nodes[id: "a"]?.sendOutcome == .unknown)
        #expect(store.fleet.nodes[id: "a"]?.message == "A message")
        #expect(store.fleet.detail?.message == "B message")
        // Sending A again is the same command: the server applies it once.
        await open("a")
        await gate.release(V2Fixture.recorded)
        await store.send(.fleet(.nodes(.element(id: "a", action: .sendTapped)))).finish()
        #expect(sent.value.map(\.0) == ["a", "a"])
        #expect(Set(sent.value.map(\.1)).count == 1)
        await open("b")
        #expect(store.fleet.nodes[id: "a"]?.message == "")
        #expect(store.fleet.nodes[id: "a"]?.sentCount == 1)
        #expect(store.fleet.detailID == "b")
        #expect(store.fleet.detail?.message == "B message")
        await store.send(.triage(.triage(.identityChanged(V2Fixture.other)))).finish()
        #expect(store.fleet.nodes.isEmpty)
        #expect(store.fleet.detailID == nil)
        for task in navigationTasks { await task.finish() }
        opened.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)))
    func authoritativeNodeRemovalPrunesOnlyOwnersWithoutOutstandingCommands() async throws {
        var editable = NodeDetailFeature.State(sessionID: "editable")
        editable.message = "Draft on removed node"
        var uncertain = NodeDetailFeature.State(sessionID: "uncertain")
        uncertain.message = "Keep recovery"
        uncertain.sendOutcome = .unknown
        var sending = NodeDetailFeature.State(sessionID: "sending")
        sending.isSending = true
        var initial = ServiceFleetFeature.State()
        initial.nodes = [editable, uncertain, sending]
        initial.detailID = "editable"
        initial.path.append(.node(.init(sessionID: "editable")))
        var empty = V2Fixture()
        empty.agents = []
        let store = Store(initialState: initial) { ServiceFleetFeature() }
        // Mount the seeded route separately from the finite projection operations.
        let navigation = store.send(.hiddenInboxChanged(["mount"]))
        await store.send(.workspaceChanged(nil)).finish()
        #expect(store.nodes.count == 3, "no projection is not proof that a node is gone")
        await store.send(.workspaceChanged(empty.workspace)).finish()
        #expect(store.nodes[id: "editable"] == nil)
        #expect(store.detailID == nil)
        #expect(store.path.count == 1, "The original gone-node screen retains Back navigation")
        guard case .node(let route) = store.path.first else { Issue.record("Node route must retain its identity"); return }
        #expect(route.sessionID == "editable")
        #expect(store.nodes[id: "uncertain"]?.message == "Keep recovery")
        #expect(store.nodes[id: "sending"]?.isSending == true)
        let pathID = try #require(store.path.ids.first)
        await store.send(.path(.popFrom(id: pathID))).finish()
        await navigation.finish()
    }

    @Test(.timeLimit(.minutes(1)), arguments: [false, true])
    func identityRemovalCancelsActualOwnerEffect(nodeOwner: Bool) async throws {
        let old = V2Fixture.identity
        let (fixture, a, _) = twoInformationItems()
        let workspace = fixture.workspace
        var initial = ServiceRootFeature.State()
        initial.triage.triage.identity = old
        initial.triage.cardIdentity = old
        initial.fleet.identity = old
        let subject = try #require(workspace.item(a).flatMap { DocumentSubject(item: $0, in: workspace) })
        var document = DocumentReviewFeature.State(subject: subject, workspace: workspace)
        document.sourceText = "Body"
        document.draft.general = "Send document"
        initial.triage.document = document
        let node = try JSONDecoder().decode(Node.self, from: Data(#"{"sessionId":"a","shortId":"a","name":"A","project":"Alondra","state":"working","stateSince":1,"isOrchestrator":false,"hasPlugin":true,"stateSource":"hook"}"#.utf8))
        var detail = NodeDetailFeature.State(sessionID: "a")
        detail.entry = FleetNode(node: node, tasks: [], pendingCount: 0)
        detail.message = "Send node"
        initial.fleet.detail = detail
        let started = AsyncStream<Void>.makeStream()
        let cancelled = AsyncStream<Void>.makeStream()
        let gate = AsyncStream<Void>.makeStream()
        let suspend: @Sendable () async -> Void = {
            await withTaskCancellationHandler {
                started.continuation.yield(())
                for await _ in gate.stream { break }
            } onCancel: {
                cancelled.continuation.yield(())
            }
        }
        let log = CommandLog()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.nodeOperations = .init(send: { _, _, _ in await suspend(); return .obsolete }, close: { _, _ in .obsolete }, threads: { _ in [] })
            $0.domainWorkspace = .scripted(log) { _ in await suspend(); return .obsolete }
        }
        let action: ServiceRootFeature.Action = nodeOwner
            ? .fleet(.nodes(.element(id: "a", action: .sendTapped)))
            : .triage(.documents(.element(id: a, action: .sendFeedbackTapped(author: ""))))
        let operation = store.send(action)
        var begins = started.stream.makeAsyncIterator()
        _ = await begins.next()
        await store.send(.triage(.triage(.identityChanged(V2Fixture.other)))).finish()
        #expect(store.fleet.nodes.isEmpty && store.triage.documents.isEmpty)
        var cancellations = cancelled.stream.makeAsyncIterator()
        _ = await cancellations.next()
        await operation.finish()
        #expect(store.notice == nil)
        #expect(store.fleet.nodes.isEmpty && store.triage.documents.isEmpty)
        started.continuation.finish(); cancelled.continuation.finish(); gate.continuation.finish()
    }
}
