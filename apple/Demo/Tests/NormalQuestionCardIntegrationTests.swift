import AlondraAttachmentsFeature
import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct NormalQuestionCardIntegrationTests {
    private func card() throws -> TriageCardFeature.State {
        let question = try JSONDecoder().decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        return TriageCardFeature.State(item: .question(question), identity: nil)
    }

    @Test
    func retryFromCardSendsOnceAndPresentsRejectionAsToastKeepingDraftAndReceipt() async throws {
        let file = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000091")!, name: "proof.txt", data: Data("proof".utf8))
        var card = try card()
        card.question?.draft.text = "Keep this answer"
        card.question?.attachmentPicker.entries = [.init(draft: file, status: .failed("Offline"))]
        var initial = ServiceRootFeature.State()
        initial.triage.cards = [card]
        let clock = TestClock()
        let uploads = LockIsolated<[AttachmentDraft]>([])
        let submissions = LockIsolated<[QuestionOperation]>([])
        let store = TestStore(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.attachmentUpload = .init(upload: { draft in
                uploads.withValue { $0.append(draft) }
                try await clock.sleep(for: .seconds(1))
                return "receipt"
            })
            $0.questionWorkService = .init(submit: { operation in
                submissions.withValue { $0.append(operation) }
                return .failed("Rejected")
            }, document: { _ in "" })
        }
        // Root + card + question + attachment integration: assert externally visible
        // presentation, preserved data and transport commands instead of child plumbing.
        store.exhaustivity = .off(showSkippedAssertions: true)
        await store.send(.triage(.cards(.element(id: card.id, action: .send))))
        await store.receive { action in
            if case .triage(.cards(.element(_, .question(.attachmentPicker(.uploadNext))))) = action { return true }
            return false
        }
        await store.send(.triage(.cards(.element(id: card.id, action: .send))))
        #expect(submissions.value.isEmpty)
        await clock.advance(by: .seconds(1))
        await store.receive { action in
            if case .triage(.cards(.element(_, .question(.submissionFinished)))) = action { return true }
            return false
        }
        #expect(uploads.value == [file])
        #expect(submissions.value == [.answer(id: 1, text: "Keep this answer", option: nil, attachments: ["receipt"])])
        let retained = try #require(store.state.triage.cards[id: card.id])
        #expect(retained.notice == nil)
        #expect(retained.question?.draft.text == "Keep this answer")
        #expect(retained.question?.draft.attachmentIDs == ["receipt"])
        #expect(retained.question?.attachmentPicker.entries.first?.draft == file)
        #expect(retained.question?.attachmentPicker.entries.first?.status == .uploaded("receipt"))
        #expect(store.state.notice?.event == .failure(couldNotSendMessage("Rejected")))
        await clock.advance(by: .seconds(6))
        await store.finish()
        #expect(store.state.notice == nil)
        #expect(store.state.triage.cards[id: card.id]?.notice == nil)
    }

    @Test
    func staleResultsCannotToastAndUncertainResultStaysVisibleUntilMatchingRecovery() async throws {
        var card = try card()
        card.question?.draft.text = "Keep this answer"
        card.question?.isSubmitting = true
        card.question?.revision = 2
        var initial = ServiceRootFeature.State()
        initial.triage.cards = [card]
        let clock = TestClock()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: { $0.continuousClock = clock }
        func send(_ action: QuestionFeature.Action) -> StoreTask {
            store.send(.triage(.cards(.element(id: card.id, action: .question(action)))))
        }
        await send(.submissionFinished(1, .failed("Stale"))).finish()
        #expect(store.notice == nil)
        #expect(store.triage.cards[id: card.id]?.question?.isSubmitting == true)
        let command = UUID(uuidString: "00000000-0000-0000-0000-000000000092")!
        await send(.submissionFinished(2, .uncertainCommand(command))).finish()
        #expect(store.notice == nil)
        #expect(store.triage.cards[id: card.id]?.notice == "The result is uncertain. Checking its status…")
        #expect(store.triage.cards[id: card.id]?.busy == true)
        await send(.commandResolved(UUID(uuidString: "00000000-0000-0000-0000-000000000093")!, rejection: "Stale")).finish()
        #expect(store.notice == nil)
        #expect(store.triage.cards[id: card.id]?.question?.needsReconciliation == true)
        let recovery = send(.commandResolved(command, rejection: "Rejected"))
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        #expect(store.triage.cards[id: card.id]?.notice == nil)
        #expect(store.triage.cards[id: card.id]?.question?.needsReconciliation == false)
        #expect(store.triage.cards[id: card.id]?.question?.draft.text == "Keep this answer")
        await clock.advance(by: .seconds(6))
        await recovery.finish()
    }
}
