import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct AttachedQuestionIntegrationTests {
    enum FailureRoute { case submission, recovery, attachmentImport }

    @Test(arguments: [false, true], [FailureRoute.submission, .recovery, .attachmentImport])
    func attachedFailuresUseGlobalToast(inCard: Bool, route: FailureRoute) async throws {
        let recovery = route == .recovery
        let decoder = JSONDecoder()
        let question = try decoder.decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let pr = try decoder.decode(PullRequest.self, from: Data(#"{"id":5,"sessionId":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","openedAt":1}"#.utf8))
        let command = UUID()
        var child = ServicePullRequestFeature.State(pr: pr, questions: [question])
        child.questionFeatures[id: 1]?.question.draft.text = "Keep the answer"
        child.questionFeatures[id: 1]?.question.isSubmitting = route == .submission
        child.questionFeatures[id: 1]?.question.needsReconciliation = recovery
        child.questionFeatures[id: 1]?.question.uncertainCommandID = recovery ? command : nil
        var initial = ServiceRootFeature.State()
        if inCard {
            var card = TriageCardFeature.State(item: .pullRequest(pr), identity: nil)
            card.pullRequest = child
            initial.triage.cards = [card]
        } else { initial.pullRequest = child }
        let clock = TestClock()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: { $0.continuousClock = clock }
        let failure: QuestionFeature.Action
        switch route {
        case .submission: failure = .submissionFinished(0, .failed("Rejected"))
        case .recovery: failure = .commandResolved(command, rejection: "Rejected")
        case .attachmentImport: failure = .attachedSubmissionFailed("Rejected")
        }
        let action = ServicePullRequestFeature.Action.attachedQuestion(.element(id: 1, action: .question(failure)))
        let task = store.send(inCard ? .triage(.cards(.element(id: "p5", action: .pullRequest(action)))) : .pullRequests(.element(id: 5, action: action)))
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        let draft = inCard ? store.triage.cards[id: "p5"]?.pullRequest?.questionFeatures[id: 1]?.question.draft : store.pullRequest?.questionFeatures[id: 1]?.question.draft
        #expect(draft?.text == "Keep the answer")
        await clock.advance(by: .seconds(6))
        await task.finish()
        #expect(store.notice == nil)
    }

    @Test
    func cardAttachedQuestionPreservesDraftAcrossSnapshotAndDoesNotDuplicateSubmission() async throws {
        let decoder = JSONDecoder()
        let question = try decoder.decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let pr = try decoder.decode(PullRequest.self, from: Data(#"{"id":5,"sessionId":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","openedAt":1}"#.utf8))
        let clock = TestClock()
        let calls = LockIsolated(0)
        let started = AsyncStream<Void>.makeStream()
        let store = Store(initialState: TriageCardFeature.State(item: .pullRequest(pr), identity: nil)) { TriageCardFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { operation in
                calls.withValue { $0 += 1 }
                started.continuation.yield(())
                #expect(operation == .answer(id: 1, text: "Keep this draft", option: nil, attachments: []))
                try? await clock.sleep(for: .seconds(1))
                return .failed("Offline")
            }, document: { _ in "" })
        }
        await store.send(.pullRequest(.contextUpdated(reports: [], questions: [question]))).finish()
        #expect(store.pullRequest?.questions == [question])
        #expect(store.pullRequest?.questionFeatures.map(\.id) == [1])
        await store.send(.pullRequest(.attachedQuestion(.element(id: 1, action: .question(.textChanged("Keep this draft")))))).finish()
        await store.send(.pullRequest(.contextUpdated(reports: [], questions: [question]))).finish()
        #expect(store.pullRequest?.questionFeatures[id: 1]?.question.draft.text == "Keep this draft")
        let submission = store.send(.pullRequest(.attachedQuestion(.element(id: 1, action: .question(.answerTapped)))))
        #expect(store.pullRequest?.questionFeatures[id: 1]?.question.isSubmitting == true)
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        await store.send(.pullRequest(.attachedQuestion(.element(id: 1, action: .question(.answerTapped))))).finish()
        await clock.advance(by: .seconds(1))
        await submission.finish()
        #expect(calls.value == 1)
        let completed = try #require(store.pullRequest?.questionFeatures[id: 1]?.question)
        #expect(!completed.isSubmitting && completed.notice == "Offline" && completed.noticeIsSubmissionFailure)
        #expect(completed.draft.text == "Keep this draft")
        await store.send(.pullRequest(.contextUpdated(reports: [], questions: []))).finish()
        #expect(store.pullRequest?.questions.isEmpty == true && store.pullRequest?.questionFeatures.isEmpty == true)
        started.continuation.finish()
    }
}
