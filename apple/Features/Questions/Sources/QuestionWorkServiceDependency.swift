import AlondraDomain
import ComposableArchitecture

private enum QuestionWorkServiceKey: DependencyKey {
    /// Composition installs the service adapter. An unwired feature cannot mutate.
    static let liveValue = QuestionWorkService(
        submit: { _ in .failed("Question service is unavailable") },
        document: { _ in throw QuestionWorkError.unavailable }
    )
    static let testValue = liveValue
}

extension DependencyValues {
    public var questionWorkService: QuestionWorkService {
        get { self[QuestionWorkServiceKey.self] }
        set { self[QuestionWorkServiceKey.self] = newValue }
    }
}

