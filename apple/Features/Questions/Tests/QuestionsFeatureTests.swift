import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraQuestionsFeature
import ComposableArchitecture
import Foundation
import Testing

enum Fixtures {
    static func workspace(_ name: String) throws -> DomainWorkspace {
        let env = ProcessInfo.processInfo.environment
        let url: URL
        if let runfiles = env["TEST_SRCDIR"] {
            url = URL(fileURLWithPath: runfiles).appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("apple/APIClient/Tests/domain-v2-wire-fixtures.json")
        } else {
            url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
                .deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("APIClient/Tests/domain-v2-wire-fixtures.json")
        }
        let all = try JSONDecoder().decode([String: DomainV2.DomainProjection].self, from: Data(contentsOf: url))
        return DomainWorkspace(try #require(all[name]))
    }

    /// A delivery that presents one DocumentRevision, as `present_result` records it.
    static func documentDelivery() throws -> DomainWorkspace {
        var projection = try workspace("pr_question").projection
        projection.documents = [.init(id: "doc", title: "Design", currentRevision: "rev-2")]
        projection.documentRevisions = [.init(id: "rev-2", documentId: "doc", number: 2,
                                              contentDigest: String(repeating: "a", count: 64), contentReference: "files/doc")]
        projection.interactions = projection.interactions.map { interaction in
            guard interaction.id == "d" else { return interaction }
            var copy = interaction
            copy.payload = .delivery(targets: [.documentRevision(documentId: "doc", revisionId: "rev-2")], isFinal: true,
                                     acceptance: nil, feedback: [])
            return copy
        }
        let action = { (kind: String) in
            DomainV2.AvailableAction(kind: kind, enabled: true, disabledReason: nil, expectedRevision: 7,
                                     presentationId: "d", expectedHead: nil)
        }
        projection.inbox = [.init(id: "interaction:d", kind: "delivery", subject: .interaction(interactionId: "d"),
                                  title: "Result", body: "Result ready", taskId: "t", requestId: "r", agentId: "a",
                                  revision: 7, order: 7, pending: true, children: [],
                                  availableActions: [action("accept_delivery"), action("submit_feedback")])]
        return DomainWorkspace(projection)
    }
}

actor Recorder {
    var intents: [DomainIntent] = []
    var commandIDs: [String] = []
    var documents: [DomainV2.DocumentRevisionId] = []
    func record(_ intent: DomainIntent, _ commandID: String) { intents.append(intent); commandIDs.append(commandID) }
    func read(_ id: DomainV2.DocumentRevisionId) { documents.append(id) }
}

func client(_ recorder: Recorder, result: DomainCommandResult) -> DomainWorkspaceClient {
    var client = DomainWorkspaceClient.inert
    client.perform = { intent, _, _, commandID in
        await recorder.record(intent, commandID)
        return result
    }
    client.documentContent = { id, _ in
        await recorder.read(id)
        return "Document body"
    }
    return client
}

private let recorded = DomainCommandResult.recorded(.init(revision: 8, changed: true, effects: [], createdIds: []))

@MainActor
struct QuestionFeatureTests {
    private func state() throws -> QuestionFeature.State {
        let workspace = try Fixtures.workspace("question")
        let item = try #require(workspace.item(for: "q"))
        return QuestionFeature.State(question: try #require(DomainQuestion(item: item, in: workspace)), workspace: workspace)
    }

    @Test
    func requiredTextInputFocusesTheReplyInsteadOfSending() async throws {
        let recorder = Recorder()
        let initial = try state()
        let store = TestStore(initialState: initial) { QuestionFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: recorded)
            $0.uuid = .incrementing
        }
        await store.send(.optionTapped("proceed")) {
            $0.draft.selectedOption = "proceed"
            $0.replyFocusRequest = 1
        }
        await store.send(.optionTapped("Continuar"))
        #expect(await recorder.intents.isEmpty)
    }

    @Test
    func textGoesToTheOptionsDeclaredInputAndCompletesOnceRecorded() async throws {
        let recorder = Recorder()
        let initial = try state()
        let store = TestStore(initialState: initial) { QuestionFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: recorded)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.textChanged(" Looks right ")) { $0.draft.text = " Looks right " }
        await store.send(.optionTapped("proceed"))
        await store.receive(\.submissionFinished)
        #expect(store.state.completed)
        #expect(store.state.draft == QuestionDraft())
        #expect(store.state.commandID == nil)
        #expect(await recorder.intents == [.answer("q", optionID: "proceed", inputs: ["explanation": .text(value: "Looks right")])])
    }

    @Test
    func staleRejectionKeepsTheDraftAndShowsAConflict() async throws {
        let recorder = Recorder()
        var initial = try state()
        initial.draft.text = "Keep me"
        initial.draft.selectedOption = "proceed"
        let store = TestStore(initialState: initial) { QuestionFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: .rejected(.init(code: "stale_revision", message: "Refresh")))
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.answerTapped)
        await store.receive(\.submissionFinished)
        #expect(store.state.draft.text == "Keep me")
        #expect(store.state.conflict != nil)
        #expect(!store.state.completed)
    }

    @Test
    func anUnknownOutcomeKeepsTheDraftAndSendingAgainReusesTheCommandID() async throws {
        let recorder = Recorder()
        var initial = try state()
        initial.draft.text = "Once"
        initial.draft.selectedOption = "proceed"
        let store = TestStore(initialState: initial) { QuestionFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: .uncertain)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.answerTapped)
        await store.receive(\.submissionFinished)
        #expect(store.state.needsReconciliation)
        #expect(store.state.draft.text == "Once")
        await store.send(.answerTapped)
        await store.receive(\.submissionFinished)
        await store.send(.textChanged("Edited"))
        await store.send(.answerTapped)
        await store.receive(\.submissionFinished)
        let ids = await recorder.commandIDs
        #expect(ids.count == 3)
        #expect(ids[0] == ids[1], "Sending the same draft again reuses its command ID")
        #expect(ids[2] != ids[0], "An edited draft is a new command")
    }
}

@MainActor
struct DocumentReviewFeatureTests {
    private func state(_ workspace: DomainWorkspace) throws -> DocumentReviewFeature.State {
        let item = try #require(workspace.item(for: "d"))
        return DocumentReviewFeature.State(subject: try #require(DocumentSubject(item: item, in: workspace)), workspace: workspace)
    }

    @Test
    func deliveryLoadsTheExactRevisionThenAcceptsIt() async throws {
        let recorder = Recorder()
        let initial = try state(Fixtures.documentDelivery())
        let store = TestStore(initialState: initial) { DocumentReviewFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: recorded)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.loaded)
        #expect(await recorder.documents == ["rev-2"])
        #expect(store.state.canAccept)
        await store.send(.reviewTapped(.approved))
        await store.receive(\.submissionFinished)
        #expect(await recorder.intents == [.acceptDelivery("d")])
        #expect(store.state.completed)
    }

    @Test
    func commentsAreSentAsOneFeedbackAndBlockAcceptance() async throws {
        let recorder = Recorder()
        let initial = try state(Fixtures.documentDelivery())
        let store = TestStore(initialState: initial) { DocumentReviewFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: recorded)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.loaded)
        await store.send(.generalChanged("Rename the section"))
        await store.send(.reviewTapped(.approved))
        #expect(await recorder.intents.isEmpty, "Accepting must not silently discard written comments")
        await store.send(.sendFeedbackTapped(author: ""))
        await store.receive(\.submissionFinished)
        #expect(await recorder.intents == [.submitFeedback("d", body: "Rename the section")])
    }

    @Test
    func informationIsRepliedToButNeverAcceptedOrReviewed() async throws {
        let recorder = Recorder()
        let workspace = try Fixtures.workspace("information")
        let item = try #require(workspace.item(for: "info"))
        let initial = DocumentReviewFeature.State(subject: try #require(DocumentSubject(item: item, in: workspace)), workspace: workspace)
        let store = TestStore(initialState: initial) { DocumentReviewFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: recorded)
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.loaded)
        #expect(!store.state.canAccept)
        #expect(!store.state.canSendFeedback)
        await store.send(.reviewTapped(.approved))
        await store.send(.sendFeedbackTapped(author: ""))
        #expect(await recorder.intents.isEmpty, "An empty reply sends nothing")
        await store.send(.generalChanged("Thanks, go on"))
        await store.send(.sendFeedbackTapped(author: ""))
        await store.receive(\.submissionFinished)
        #expect(await recorder.intents == [.replyToInformation("info", body: "Thanks, go on")])
    }
}
