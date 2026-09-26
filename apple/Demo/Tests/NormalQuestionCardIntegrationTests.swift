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
        let uploadStarted = AsyncStream<Void>.makeStream()
        let releaseUpload = AsyncStream<Void>.makeStream()
        let rejectionReduced = AsyncStream<Void>.makeStream()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                // Observation barrier only: the production root has already reduced
                // the real transport response when this signal is delivered.
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .triage(.cards(.element(_, .question(.submissionFinished)))) = action {
                        rejectionReduced.continuation.yield(())
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.attachmentUpload = .init(upload: { draft in
                uploads.withValue { $0.append(draft) }
                uploadStarted.continuation.yield(())
                for await _ in releaseUpload.stream { break }
                return "receipt"
            })
            $0.questionWorkService = .init(submit: { operation in
                submissions.withValue { $0.append(operation) }
                return .failed("Rejected")
            }, document: { _ in "" })
        }
        // Hold the real upload effect while another Send reaches the production root.
        let submission = store.send(.triage(.cards(.element(id: card.id, action: .send))))
        var started = uploadStarted.stream.makeAsyncIterator()
        _ = await started.next()
        await store.send(.triage(.cards(.element(id: card.id, action: .send)))).finish()
        #expect(submissions.value.isEmpty)
        releaseUpload.continuation.yield(())
        var rejected = rejectionReduced.stream.makeAsyncIterator()
        _ = await rejected.next()
        #expect(uploads.value == [file])
        #expect(submissions.value == [.answer(id: 1, text: "Keep this answer", option: nil, attachments: ["receipt"])])
        let retained = try #require(store.triage.cards[id: card.id])
        #expect(retained.notice == nil)
        #expect(retained.question?.draft.text == "Keep this answer")
        #expect(retained.question?.draft.attachmentIDs == ["receipt"])
        #expect(retained.question?.attachmentPicker.entries.first?.draft == file)
        #expect(retained.question?.attachmentPicker.entries.first?.status == .uploaded("receipt"))
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        await clock.advance(by: .seconds(6))
        await submission.finish()
        #expect(store.notice == nil)
        #expect(store.triage.cards[id: card.id]?.notice == nil)
        uploadStarted.continuation.finish()
        releaseUpload.continuation.finish()
        rejectionReduced.continuation.finish()
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
