import AlondraDomain
import AlondraShared
import ComposableArchitecture

public enum PRPostFailureClassifier {
    public static let isUnknown: @Sendable (any Error) -> Bool = { error in
        error is PRPostDispatchUncertain || error.isTransient
    }
}

private enum PullRequestOperationsKey: DependencyKey {
    static let liveValue = PullRequestOperations(
        diff: { _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        merge: { _, _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        resolveConflicts: { _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        setAside: { _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        review: { _, _, _, _, _ in preconditionFailure("Install PullRequestOperations at the app composition root") }
    )
    static let testValue = liveValue
}

public extension DependencyValues {
    var pullRequestOperations: PullRequestOperations {
        get { self[PullRequestOperationsKey.self] }
        set { self[PullRequestOperationsKey.self] = newValue }
    }
}
