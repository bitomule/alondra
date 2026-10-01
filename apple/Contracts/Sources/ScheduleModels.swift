import Foundation

/// The schedule screens' value. It is built from the v2 Schedule projection; the server owns
/// `next_run` and the last run.
public struct ScheduledTask: Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public let text: String
    public let project: String?
    public let options: [String: OptionChoice]
    public let disallowedTools: [String]
    public let frequency: String
    public let hour: Int
    public let minute: Int
    public let weekday: Int?
    public let interval: Int?
    public let startAt: Int64?
    public let enabled: Bool
    public let nextRun: Int64?
    public let lastRun: Int64?
    public let lastRequest: String?
    public let lastError: String?
    public let status: String
    public let revision: Int

}

public struct ScheduledTaskDraft: Equatable, Sendable {
    public var title: String
    public var text: String
    public var project: String?
    public var options: [String: OptionChoice]
    public var disallowedTools: [String]
    public var frequency: String
    public var hour: Int
    public var minute: Int
    public var weekday: Int?
    public var interval: Int?
    public var startAt: Int64?
    public var enabled: Bool
    public var revision: Int

    public init(_ task: ScheduledTask? = nil) {
        title = task?.title ?? ""
        text = task?.text ?? ""
        project = task?.project
        options = task?.options ?? [:]
        disallowedTools = task?.disallowedTools ?? []
        frequency = task?.frequency ?? "daily"
        hour = task?.hour ?? 9
        minute = task?.minute ?? 0
        weekday = task?.weekday ?? 1
        interval = task?.interval ?? 1
        startAt = task?.startAt ?? Int64((Date.now.addingTimeInterval(3600).timeIntervalSince1970 * 1000).rounded())
        enabled = task?.enabled ?? true
        revision = task?.revision ?? 0
    }
}
