import AlondraContracts

/// Review choices share the original phone behavior with platform presentations.
public struct DocumentReviewChoices: Sendable {
    public init() {}
    public func approveOption(_ question: Question) -> String? {
        if question.isPlan { return AnswerOption.approve }
        return question.options.map(\.label).first { $0.hasPrefix(AnswerOption.approve) || $0.hasPrefix("Aprobar") || $0 == AnswerOption.allow }
    }
    public func changeOption(_ question: Question) -> String? {
        if question.isPlan { return AnswerOption.requestChanges }
        return question.options.map(\.label).first { $0.hasPrefix(AnswerOption.requestChanges) || $0.hasPrefix("Pedir cambios") || $0.hasPrefix("Cambiar") || $0 == AnswerOption.deny }
    }
    public func requiresText(_ option: QuestionOption) -> Bool {
        AnswerQuestionUseCase.needsExplanation(option) || [AnswerOption.requestChanges, AnswerOption.deny].contains(option.label)
    }
}
