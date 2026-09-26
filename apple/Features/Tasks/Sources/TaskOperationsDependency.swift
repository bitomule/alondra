import AlondraDomain
import ComposableArchitecture

private enum TaskOperationsKey: DependencyKey {
    static let liveValue = TaskOperations(
        cancel: { _ in preconditionFailure("Install TaskOperations at the app composition root") },
        reopen: { _ in preconditionFailure("Install TaskOperations at the app composition root") },
        archiveCompleted: { preconditionFailure("Install TaskOperations at the app composition root") },
        details: { _ in preconditionFailure("Install TaskOperations at the app composition root") }
    )
    static let testValue = liveValue
}

public extension DependencyValues {
    var taskOperations: TaskOperations {
        get { self[TaskOperationsKey.self] }
        set { self[TaskOperationsKey.self] = newValue }
    }
}
