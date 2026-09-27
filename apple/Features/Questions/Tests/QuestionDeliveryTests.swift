import AlondraContracts
import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@MainActor struct QuestionDeliveryTests {
    @Test(arguments: [false, true]) func directAndRecoveredUncertainHandoffKeepDraftAndBlockAnotherAnswer(recovered: Bool) async throws {
        let question = try JSONDecoder().decode(Question.self, from: Data(#"{"id":42,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let command = UUID(uuidString: "00000000-0000-0000-0000-000000000001")!
        var initial = QuestionFeature.State(question: question)
        initial.draft.text = "Keep this draft"
        initial.isSubmitting = !recovered
        initial.needsReconciliation = recovered
        initial.uncertainCommandID = recovered ? command : nil
        let store = TestStore(initialState: initial) { QuestionFeature() }
        let action: QuestionFeature.Action = recovered ? .commandDeliveryUnconfirmed(command, attemptID: 77) : .submissionFinished(0, .deliveryUnconfirmed(attemptID: 77))
        await store.send(action) {
            $0.isSubmitting = false; $0.uncertainCommandID = nil
            $0.needsReconciliation = true; $0.deliveryUnconfirmed = true; $0.deliveryAttemptID = 77
            $0.notice = DeliveryHandoffUnconfirmed.message
        }
        await store.send(.answerTapped)
        await store.send(.reconciled(cardStillOpen: false))
        #expect(!store.state.completed && store.state.draft.text == "Keep this draft")
        await store.send(.deliveryDecisionResolved(retry: true)) {
            $0.needsReconciliation = false; $0.deliveryUnconfirmed = false; $0.deliveryAttemptID = nil; $0.notice = nil
        }
        #expect(store.state.draft.text == "Keep this draft" && !store.state.completed)
        await store.finish()
    }
}
