import AlondraQuestionsFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct DocumentReviewChoiceTests {
    @Test
    func reviewChoiceRequestsExplanationThenSendsFeedbackWithTheSelectedOption() async throws {
        let question = try JSONDecoder().decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Review","options":[{"label":"Deny","description":"Write the reason"}],"links":[],"attachments":[],"origin":"permission","status":"open","askedAt":1}"#.utf8))
        var initial = DocumentReviewFeature.State(subject: .question(question))
        initial.sourceText = "Document"
        let store = TestStore(initialState: initial) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { operation in
                guard case .answer(let id, let text, let option, let attachments) = operation else {
                    Issue.record("Expected the original review answer command")
                    return .failed("Unexpected operation")
                }
                #expect(id == 1)
                #expect(option == "Deny")
                #expect(text.contains("Keep the existing layout"))
                #expect(attachments.isEmpty)
                return .confirmed
            }, document: { _ in "" })
        }
        let option = try #require(question.options.first)
        await store.send(.chooseReviewOption(option, author: "David")) { $0.generalFocusRequest = 1 }
        await store.send(.generalChanged("Keep the existing layout")) { $0.draft.general = "Keep the existing layout" }
        await store.send(.chooseReviewOption(option, author: "David")) { $0.draft.selectedOption = "Deny" }
        await store.receive(\.sendFeedbackTapped) { $0.isSubmitting = true }
        await store.receive(\.submissionFinished) { $0.isSubmitting = false; $0.completed = true; $0.draft = DocumentDraft() }
    }
}
