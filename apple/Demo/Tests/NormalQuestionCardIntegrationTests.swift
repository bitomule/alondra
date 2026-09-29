import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct NormalQuestionCardIntegrationTests {
    nonisolated static let receipt = DomainV2.AttachmentRef(id: "receipt", name: "proof.txt", contentDigest: Data("proof".utf8).sha256Hex,
                                                contentReference: "ref", bytes: 5)

    private func card() -> TriageCardFeature.State {
        var fixture = V2Fixture()
        let item = fixture.question("q1")
        var card = TriageCardFeature.State(item: item, workspace: fixture.workspace, identity: nil)
        card.question?.draft.selectedOption = "reply"
        card.question?.draft.text = "Keep this answer"
        return card
    }

    @Test
    func retryFromCardSendsOnceAndPresentsRejectionAsToastKeepingDraftAndReceipt() async throws {
        let file = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000091")!, name: "proof.txt", data: Data("proof".utf8))
        var card = card()
        card.question?.attachmentPicker.entries = [.init(draft: file, status: .failed("Offline"))]
        var initial = ServiceRootFeature.State()
        initial.triage.cards = [card]
        let clock = TestClock()
        let uploads = LockIsolated<[AttachmentDraft]>([])
        let log = CommandLog()
        let uploadStarted = AsyncStream<Void>.makeStream()
        let releaseUpload = AsyncStream<Void>.makeStream()
        let rejectionReduced = AsyncStream<Void>.makeStream()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                // Observation barrier only: the production root has already reduced the
                // real transport response when this signal is delivered.
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
                return Self.receipt
            })
            $0.domainWorkspace = .scripted(log) { _ in V2Fixture.rejected() }
        }
        // Hold the real upload effect while another Send reaches the production root.
        let submission = store.send(.triage(.cards(.element(id: card.id, action: .send))))
        var started = uploadStarted.stream.makeAsyncIterator()
        _ = await started.next()
        await store.send(.triage(.cards(.element(id: card.id, action: .send)))).finish()
        #expect(log.count == 0)
        releaseUpload.continuation.yield(())
        var rejected = rejectionReduced.stream.makeAsyncIterator()
        _ = await rejected.next()
        #expect(uploads.value == [file])
        #expect(log.intents == [.answer("q1", optionID: "reply", inputs: ["text": .text(value: "Keep this answer")], attachments: [Self.receipt])])
        let retained = try #require(store.triage.cards[id: card.id])
        #expect(retained.notice == nil)
        #expect(retained.question?.draft.text == "Keep this answer")
        #expect(retained.question?.attachmentPicker.stored == [Self.receipt])
        #expect(retained.question?.attachmentPicker.entries.first?.draft == file)
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
    func staleResultsCannotToastAndAnUnknownResultIsRetriedWithTheSameCommand() async throws {
        let card = card()
        var initial = ServiceRootFeature.State()
        initial.triage.cards = [card]
        let clock = TestClock()
        let log = CommandLog()
        let results = LockIsolated<[DomainCommandResult]>([.uncertain, V2Fixture.rejected()])
        let finished = AsyncStream<Void>.makeStream()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .triage(.cards(.element(_, .question(.submissionFinished)))) = action { finished.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.uuid = .incrementing
            $0.domainWorkspace = .scripted(log) { _ in results.withValue { $0.removeFirst() } }
        }
        func send(_ action: QuestionFeature.Action) -> StoreTask {
            store.send(.triage(.cards(.element(id: card.id, action: .question(action)))))
        }
        var completions = finished.stream.makeAsyncIterator()
        await send(.answerTapped).finish()
        _ = await completions.next()
        #expect(store.notice == nil)
        let unknown = try #require(store.triage.cards[id: card.id])
        #expect(unknown.notice == "We couldn’t confirm the result. Your draft is kept; sending it again won’t apply it twice.")
        #expect(unknown.question?.needsReconciliation == true)
        #expect(unknown.question?.draft.text == "Keep this answer")
        await send(.submissionFinished(99, V2Fixture.rejected("Stale"))).finish()
        _ = await completions.next()
        #expect(store.notice == nil, "a result for another attempt is not this card's error")
        let retry = send(.answerTapped)
        _ = await completions.next()
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        #expect(log.count == 2)
        #expect(Set(log.commandIDs).count == 1)
        #expect(store.triage.cards[id: card.id]?.notice == nil)
        #expect(store.triage.cards[id: card.id]?.question?.needsReconciliation == false)
        #expect(store.triage.cards[id: card.id]?.question?.draft.text == "Keep this answer")
        await clock.advance(by: .seconds(6))
        await retry.finish()
        finished.continuation.finish()
    }
}
