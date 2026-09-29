import AlondraAttachmentsFeature
import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct AttachedQuestionIntegrationTests {
    enum FailureRoute: CaseIterable { case submission, attachmentUpload }

    private let file = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000071")!, name: "proof.txt", data: Data("proof".utf8))

    private func card(questions: [String] = ["q1"]) -> (TriageCardFeature.State, DomainWorkspace) {
        var fixture = V2Fixture()
        let root = fixture.pullRequest(5, questions: questions)
        return (TriageCardFeature.State(item: root, workspace: fixture.workspace, identity: V2Fixture.workIdentity), fixture.workspace)
    }

    @Test(arguments: [false, true], FailureRoute.allCases)
    func attachedFailuresUseGlobalToast(inCard: Bool, route: FailureRoute) async throws {
        var (card, _) = card()
        var child = try #require(card.pullRequest)
        child.questionFeatures[id: "q1"]?.question.draft.selectedOption = "reply"
        child.questionFeatures[id: "q1"]?.question.draft.text = "Keep the answer"
        switch route {
        case .submission:
            child.questionFeatures[id: "q1"]?.question.isSubmitting = true
        case .attachmentUpload:
            child.questionFeatures[id: "q1"]?.question.attachmentPicker.entries = [.init(draft: file, status: .uploading)]
            child.questionFeatures[id: "q1"]?.question.pendingSubmission = .answer
        }
        var initial = ServiceRootFeature.State()
        if inCard {
            card.pullRequest = child
            initial.triage.cards = [card]
        } else { initial.pullRequest = child }
        let clock = TestClock()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: { $0.continuousClock = clock }
        let failure: QuestionFeature.Action = switch route {
        case .submission: .submissionFinished(0, V2Fixture.rejected())
        case .attachmentUpload: .attachmentPicker(.uploaded(file.id, .failure(AttachmentFailure(message: "Rejected"))))
        }
        let action = ServicePullRequestFeature.Action.attachedQuestion(.element(id: "q1", action: .question(failure)))
        let task = store.send(inCard ? .triage(.cards(.element(id: card.id, action: .pullRequest(action)))) : .pullRequests(.element(id: "pr-5", action: action)))
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        let question = inCard ? store.triage.cards[id: card.id]?.pullRequest?.questionFeatures[id: "q1"]?.question
            : store.pullRequest?.questionFeatures[id: "q1"]?.question
        #expect(question?.draft.text == "Keep the answer")
        #expect(question?.pendingSubmission == nil && question?.isSubmitting == false)
        await clock.advance(by: .seconds(6))
        await task.finish()
        #expect(store.notice == nil)
    }

    @Test
    func cardAttachedQuestionPreservesDraftAcrossRefreshAndDoesNotDuplicateSubmission() async throws {
        let (card, workspace) = card()
        let pr = try #require(card.pullRequest?.lastKnownCard)
        let log = CommandLog()
        let gate = CommandGate()
        let store = Store(initialState: card) { TriageCardFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in await gate.run() }
        }
        #expect(store.pullRequest?.questions.map(\.id) == ["q1"])
        #expect(store.pullRequest?.questionFeatures.map(\.id) == ["q1"])
        func question(_ action: QuestionFeature.Action) -> TriageCardFeature.Action {
            .pullRequest(.attachedQuestion(.element(id: "q1", action: .question(action))))
        }
        await store.send(question(.optionSelected("reply"))).finish()
        await store.send(question(.textChanged("Keep this draft"))).finish()
        await store.send(.pullRequest(.updated(pr, workspace))).finish()
        #expect(store.pullRequest?.questionFeatures[id: "q1"]?.question.draft.text == "Keep this draft")
        let submission = store.send(question(.answerTapped))
        #expect(store.pullRequest?.questionFeatures[id: "q1"]?.question.isSubmitting == true)
        await gate.waitUntilStarted()
        await store.send(question(.answerTapped)).finish()
        await gate.release(V2Fixture.rejected("Offline"))
        await submission.finish()
        #expect(log.count == 1)
        #expect(log.intents.first == .answer("q1", optionID: "reply", inputs: ["text": .text(value: "Keep this draft")]))
        let completed = try #require(store.pullRequest?.questionFeatures[id: "q1"]?.question)
        #expect(!completed.isSubmitting && completed.notice == "Offline" && completed.noticeIsSubmissionFailure)
        #expect(completed.draft.text == "Keep this draft")
        let (answered, after) = self.card(questions: [])
        await store.send(.pullRequest(.updated(answered.pullRequest?.lastKnownCard, after))).finish()
        #expect(store.pullRequest?.questions.isEmpty == true && store.pullRequest?.questionFeatures.isEmpty == true)
    }
}
