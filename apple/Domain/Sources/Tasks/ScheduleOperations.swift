import AlondraContracts

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
