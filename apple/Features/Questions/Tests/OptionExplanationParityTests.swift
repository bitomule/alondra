import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct OptionExplanationParityTests {
    enum Scenario: CaseIterable { case normalDeny, permissionDeny, planChanges, explicitWriting }
    private func question(_ scenario: Scenario) throws -> Question {
        let label = scenario == .planChanges ? AnswerOption.requestChanges : AnswerOption.deny
        var option: [String: Any] = ["label": label]
        if scenario == .explicitWriting { option["description"] = "Write your explanation" }
        let json: [String: Any] = ["id": 1, "sessionId": "s", "title": "Question", "detail": "Body", "options": [option],
            "links": [], "attachments": [], "origin": scenario == .planChanges ? "plan" : scenario == .permissionDeny ? "permission" : "ask",
            "status": "open", "askedAt": 1]
        return try JSONDecoder().decode(Question.self, from: JSONSerialization.data(withJSONObject: json))
    }

    @Test(arguments: Scenario.allCases, [false, true])
    func documentOptionMatchesOriginalPlatformAndStillRequiresExplicitWriting(scenario: Scenario, phone: Bool) async throws {
        let question = try question(scenario)
        let option = try #require(question.options.first)
        var initial = DocumentReviewFeature.State(subject: .question(question))
        initial.sourcePolicy = phone ? .phone : .allSections
        initial.sourceText = "Body"
        let operations = LockIsolated<[QuestionOperation]>([])
        let store = Store(initialState: initial) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = .init(submit: { operation in operations.withValue { $0.append(operation) }; return .failed("Rejected") }, document: { _ in "Body" })
        }
        await store.send(.chooseReviewOption(option, author: "")).finish()
        if phone || scenario == .explicitWriting {
            #expect(operations.value.isEmpty)
            #expect(store.generalFocusRequest == 1)
            #expect(store.draft.selectedOption == option.label)
        } else {
            #expect(operations.value == [.answer(id: question.id, text: option.label, option: option.label, attachments: [])])
            #expect(store.generalFocusRequest == 0)
        }
    }

    @Test(arguments: [false, true])
    func attachedPermissionOptionUsesItsPlatformPolicy(phone: Bool) async throws {
        let question = try question(.permissionDeny)
        var initial = QuestionFeature.State(question: question)
        initial.explanationPolicy = phone ? .phone : .desktop
        let operations = LockIsolated<[QuestionOperation]>([])
        let store = Store(initialState: initial) { QuestionFeature() } withDependencies: {
            $0.questionWorkService = .init(submit: { operation in operations.withValue { $0.append(operation) }; return .failed("Rejected") }, document: { _ in "" })
        }
        await store.send(phone ? .attachedOptionTapped(AnswerOption.deny) : .optionTapped(AnswerOption.deny)).finish()
        if phone {
            #expect(operations.value.isEmpty && store.replyFocusRequest == 1)
        } else {
            #expect(operations.value == [.answer(id: 1, text: AnswerOption.deny, option: AnswerOption.deny, attachments: [])])
            #expect(store.replyFocusRequest == 0)
        }
    }
}
