import AlondraShared
import ComposableArchitecture

public struct ScheduleOperations: Sendable {
    public var list: @Sendable () async throws -> [ScheduledTask]
    public var save: @Sendable (ScheduledTaskDraft, String?) async -> TaskMutationOutcome
    public var run: @Sendable (String) async -> TaskMutationOutcome
    public var delete: @Sendable (String) async -> TaskMutationOutcome
    public var projects: @Sendable () async throws -> [String]
    public var options: @Sendable (String?) async throws -> [OptionSpec]

    public init(
        list: @escaping @Sendable () async throws -> [ScheduledTask],
        save: @escaping @Sendable (ScheduledTaskDraft, String?) async -> TaskMutationOutcome,
        run: @escaping @Sendable (String) async -> TaskMutationOutcome,
        delete: @escaping @Sendable (String) async -> TaskMutationOutcome,
        projects: @escaping @Sendable () async throws -> [String],
        options: @escaping @Sendable (String?) async throws -> [OptionSpec]
    ) {
        self.list = list
        self.save = save
        self.run = run
        self.delete = delete
        self.projects = projects
        self.options = options
    }
}

private enum ScheduleOperationsKey: DependencyKey {
    static let liveValue = ScheduleOperations(
        list: { preconditionFailure("Install ScheduleOperations at the app composition root") },
        save: { _, _ in preconditionFailure("Install ScheduleOperations at the app composition root") },
        run: { _ in preconditionFailure("Install ScheduleOperations at the app composition root") },
        delete: { _ in preconditionFailure("Install ScheduleOperations at the app composition root") },
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
