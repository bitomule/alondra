import AlondraDomain
import ComposableArchitecture

private enum ScheduleOperationsKey: DependencyKey {
    static let liveValue = ScheduleOperations(
        list: { preconditionFailure("Install ScheduleOperations at the app composition root") },
        save: { _, _, _ in preconditionFailure("Install ScheduleOperations at the app composition root") },
        run: { _, _ in preconditionFailure("Install ScheduleOperations at the app composition root") },
        delete: { _, _ in preconditionFailure("Install ScheduleOperations at the app composition root") },
        projects: { preconditionFailure("Install ScheduleOperations at the app composition root") },
        options: { _ in preconditionFailure("Install ScheduleOperations at the app composition root") }
    )
    static let testValue = liveValue
}

public extension DependencyValues {
    var scheduleOperations: ScheduleOperations {
        get { self[ScheduleOperationsKey.self] }
        set { self[ScheduleOperationsKey.self] = newValue }
    }
}
