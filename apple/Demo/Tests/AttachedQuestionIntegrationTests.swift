import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct AttachedQuestionIntegrationTests {
    @Test
    func cardAttachedQuestionPreservesDraftAcrossSnapshotAndDoesNotDuplicateSubmission() async throws {
        let decoder = JSONDecoder()
        let question = try decoder.decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let pr = try decoder.decode(PullRequest.self, from: Data(#"{"id":5,"sessionId":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","openedAt":1}"#.utf8))
        let clock = TestClock()
        let store = TestStore(initialState: TriageCardFeature.State(item: .pullRequest(pr), identity: nil)) { TriageCardFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { operation in
                #expect(operation == .answer(id: 1, text: "Keep this draft", option: nil, attachments: []))
                try? await clock.sleep(for: .seconds(1))
                return .failed("Offline")
            }, document: { _ in "" })
        }
        await store.send(.pullRequest(.contextUpdated(reports: [], questions: [question]))) {
            $0.pullRequest?.questions = [question]
            $0.pullRequest?.questionFeatures = [AttachedPRQuestionFeature.State(question)]
        }
        await store.send(.pullRequest(.attachedQuestion(.element(id: 1, action: .question(.textChanged("Keep this draft")))))) {
            $0.pullRequest?.questionFeatures[id: 1]?.question.draft.text = "Keep this draft"
        }
        await store.send(.pullRequest(.contextUpdated(reports: [], questions: [question])))
        await store.receive(\.pullRequest.attachedQuestion)
        await store.send(.pullRequest(.attachedQuestion(.element(id: 1, action: .question(.answerTapped))))) {
            $0.pullRequest?.questionFeatures[id: 1]?.question.isSubmitting = true
        }
        // A second tap while the effect is suspended cannot start another submission.
        await store.send(.pullRequest(.attachedQuestion(.element(id: 1, action: .question(.answerTapped)))))
        await clock.advance(by: .seconds(1))
        await store.receive(\.pullRequest.attachedQuestion) {
            $0.pullRequest?.questionFeatures[id: 1]?.question.isSubmitting = false
            $0.pullRequest?.questionFeatures[id: 1]?.question.notice = "Offline"
        }
        #expect(store.state.pullRequest?.questionFeatures[id: 1]?.question.draft.text == "Keep this draft")
        await store.send(.pullRequest(.contextUpdated(reports: [], questions: []))) {
            $0.pullRequest?.questions = []
            $0.pullRequest?.questionFeatures = []
        }
        await store.finish()
    }
}
