import AlondraDomain
import ComposableArchitecture

private enum NodeOperationsKey: DependencyKey {
    static let liveValue = NodeOperations(
        send: { _, _ in preconditionFailure("Install NodeOperations at the app composition root") },
        close: { _ in preconditionFailure("Install NodeOperations at the app composition root") },
        threads: { _ in preconditionFailure("Install NodeOperations at the app composition root") }
    )
    static let testValue = liveValue
}

public extension DependencyValues {
    var nodeOperations: NodeOperations {
        get { self[NodeOperationsKey.self] }
        set { self[NodeOperationsKey.self] = newValue }
    }
}
