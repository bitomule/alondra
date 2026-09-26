import AlondraAttachmentsFeature
import AlondraCommands
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
    private func question(_ id: Int) throws -> Question {
        try JSONDecoder().decode(Question.self, from: Data("""
        {"id":\(id),"sessionId":"s","title":"Question \(id)","detail":"Body","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}
        """.utf8))
    }

    @Test(.timeLimit(.minutes(1)))
    func navigatingDuringFinalKeepsItsOwnerAndOtherDraft() async throws {
        let a = try question(1), b = try question(2)
        var initial = ServiceTriageFeature.State()
        initial.triage.items = [.question(a), .question(b)]
        let started = AsyncStream<Void>.makeStream()
        let release = AsyncStream<Void>.makeStream()
        let sent = LockIsolated(0)
        let store = Store(initialState: initial) { ServiceTriageFeature() } withDependencies: {
            $0.questionWorkService = .init(submit: { _ in
                sent.withValue { $0 += 1 }
                started.continuation.yield(())
                for await _ in release.stream { break }
                return .confirmed
            }, document: { _ in "Body" })
        }
        await store.send(.open("q1")).finish()
        await store.send(.documents(.element(id: "q1", action: .appeared))).finish()
        await store.send(.documents(.element(id: "q1", action: .generalChanged("A draft")))).finish()
        await store.send(.open("q2")).finish()
        await store.send(.documents(.element(id: "q2", action: .generalChanged("B draft")))).finish()
        await store.send(.open("q1")).finish()
        #expect(store.document?.draft.general == "A draft")
        let submission = store.send(.documents(.element(id: "q1", action: .submitFeedback(author: ""))))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        await store.send(.open("q2")).finish()
        #expect(store.documents[id: "q1"]?.isSubmitting == true)
        #expect(store.document?.draft.general == "B draft")
        // Reselecting A must not create a fresh idle owner that permits a second send.
        await store.send(.open("q1")).finish()
        await store.send(.documents(.element(id: "q1", action: .submitFeedback(author: "")))).finish()
        await store.send(.open("q2")).finish()
        release.continuation.yield(())
        await submission.finish()
        #expect(sent.value == 1)
        #expect(store.documents[id: "q1"] == nil)
        #expect(store.triage.hiddenIDs.contains("q1"))
        #expect(store.documentID == "q2")
        #expect(store.document?.draft.general == "B draft")
        #expect(store.document?.completed == false)
        started.continuation.finish(); release.continuation.finish()
    }

    @Test
    func leavingPendingUploadRetainsReceiptWithoutSendingAndIdentityClearsOwners() async throws {
        let a = try question(1), b = try question(2)
        var initial = ServiceTriageFeature.State()
        initial.triage.items = [.question(a), .question(b)]
        var owner = DocumentReviewFeature.State(subject: .question(a))
        let file = AttachmentDraft(name: "proof.txt", data: Data("proof".utf8))
        owner.draft.general = "Keep me"
        owner.sourceText = "Body"
        owner.attachmentPicker.entries = [.init(draft: file, status: .uploading)]
        owner.pendingSubmission = .awaitingReceipts
        owner.pendingReview = .feedback("")
        initial.document = owner
        let sent = LockIsolated(0)
        let store = Store(initialState: initial) { ServiceTriageFeature() } withDependencies: {
            $0.questionWorkService = .init(submit: { _ in sent.withValue { $0 += 1 }; return .confirmed }, document: { _ in "" })
        }
        await store.send(.open("q2")).finish()
        await store.send(.documents(.element(id: "q1", action: .attachmentPicker(.uploaded(file.id, .success("stored-proof")))))).finish()
        await store.send(.open("q1")).finish()
        #expect(store.document?.draft.general == "Keep me")
        #expect(store.document?.attachmentPicker.storedIDs == ["stored-proof"])
        #expect(store.document?.pendingSubmission == nil)
        #expect(sent.value == 0)
        let identity = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "other", credentialRevision: "two", generation: 2)
        await store.send(.triage(.identityChanged(identity))).finish()
        #expect(store.documents.isEmpty)
        #expect(store.documentID == nil)
    }
    @Test(.timeLimit(.minutes(1)))
    func nodeFinalAndLateRecoveryRemainWithOriginalNode() async throws {
        func node(_ id: String) throws -> Node {
            try JSONDecoder().decode(Node.self, from: Data("""
            {"sessionId":"\(id)","shortId":"\(id)","name":"\(id)","project":"Alondra","state":"working","stateSince":1,"isOrchestrator":false,"hasPlugin":true,"stateSource":"hook"}
            """.utf8))
        }
        let a = try node("a"), b = try node("b")
        let commandID = UUID()
        var initial = ServiceRootFeature.State()
        initial.fleet.fleet.snapshot = FleetSnapshot(sequence: 1, tick: 1, orchestrator: nil,
            sections: [.init(name: "Alondra", nodes: [.init(node: a, tasks: [], pendingCount: 0), .init(node: b, tasks: [], pendingCount: 0)])])
        let started = AsyncStream<Void>.makeStream()
        let release = AsyncStream<Void>.makeStream()
        let sent = LockIsolated<[String]>([])
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.nodeOperations = .init(send: { id, _ in
                sent.withValue { $0.append(id) }
                started.continuation.yield(())
                for await _ in release.stream { break }
                return .unknown(commandID)
            }, close: { _ in .rejected("Unexpected close") }, threads: { _ in [] })
        }
        await store.send(.fleet(.openNode("a"))).finish()
        await store.send(.fleet(.nodes(.element(id: "a", action: .messageChanged("A message"))))).finish()
        await store.send(.fleet(.openNode("b"))).finish()
        await store.send(.fleet(.nodes(.element(id: "b", action: .messageChanged("B message"))))).finish()
        await store.send(.fleet(.openNode("a"))).finish()
        #expect(store.fleet.detail?.message == "A message")
        let submission = store.send(.fleet(.nodes(.element(id: "a", action: .sendTapped))))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        await store.send(.fleet(.openNode("b"))).finish()
        release.continuation.yield(())
        await submission.finish()
        #expect(store.fleet.nodes[id: "a"]?.sendOutcome == .unknown(commandID))
        #expect(store.fleet.detail?.message == "B message")
        await store.send(.fleet(.openNode("a"))).finish()
        await store.send(.fleet(.nodes(.element(id: "a", action: .sendTapped)))).finish()
        #expect(sent.value == ["a"])
        await store.send(.fleet(.openNode("b"))).finish()
        let scope = CommandScope(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        let resolution = CommandResolution(command: .init(id: commandID, scope: scope, kind: .send("a"), itemID: nil), status: 200)
        await store.send(.commandResolved(resolution)).finish()
        #expect(store.fleet.nodes[id: "a"]?.message == "")
        #expect(store.fleet.nodes[id: "a"]?.sentCount == 1)
        #expect(store.fleet.detailID == "b")
        #expect(store.fleet.detail?.message == "B message")
        #expect(sent.value == ["a"])
        let other = TriageIdentity(server: scope.server, deviceID: "other", credentialRevision: "two", generation: 2)
        await store.send(.triage(.triage(.identityChanged(other)))).finish()
        #expect(store.fleet.nodes.isEmpty)
        #expect(store.fleet.detailID == nil)
        started.continuation.finish(); release.continuation.finish()
    }

    @Test
    func journalFindsOffscreenDocumentByOutstandingCommand() async throws {
        let a = try question(1), b = try question(2)
        let commandID = UUID()
        var first = DocumentReviewFeature.State(subject: .question(a))
        first.draft.general = "A draft"
        first.needsReconciliation = true
        first.uncertainCommandID = commandID
        var second = DocumentReviewFeature.State(subject: .question(b))
        second.draft.general = "B draft"
        var initial = ServiceRootFeature.State()
        initial.triage.documents = [first, second]
        initial.triage.documentID = "q2"
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.questionWorkService = .init(submit: { _ in Issue.record("Recovery must not replay send"); return .obsolete }, document: { _ in "" })
        }
        let scope = CommandScope(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        await store.send(.commandResolved(CommandResolution(command: .init(id: UUID(), scope: scope, kind: .answer(1), itemID: nil), status: 200))).finish()
        #expect(store.triage.documents[id: "q1"]?.needsReconciliation == true)
        await store.send(.commandResolved(CommandResolution(command: .init(id: commandID, scope: scope, kind: .answer(1), itemID: nil), status: 200))).finish()
        #expect(store.triage.documents[id: "q1"] == nil)
        #expect(store.triage.triage.hiddenIDs.contains("q1"))
        #expect(store.triage.documentID == "q2")
        #expect(store.triage.document?.draft.general == "B draft")
    }

}
