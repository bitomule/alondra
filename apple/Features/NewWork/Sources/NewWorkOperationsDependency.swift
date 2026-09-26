import AlondraDomain
import ComposableArchitecture

private enum NewWorkOperationsKey: DependencyKey {
    static let liveValue = NewWorkOperations(
        projects: { preconditionFailure("Install NewWorkOperations at the app composition root") },
        options: { _ in preconditionFailure("Install NewWorkOperations at the app composition root") },
        issues: { _ in preconditionFailure("Install NewWorkOperations at the app composition root") },
        captureSubmissionScope: { preconditionFailure("Install NewWorkOperations at the app composition root") }
    )
    static let testValue = liveValue
}

public extension DependencyValues {
    var newWorkOperations: NewWorkOperations {
        get { self[NewWorkOperationsKey.self] }
        set { self[NewWorkOperationsKey.self] = newValue }
    }
}
