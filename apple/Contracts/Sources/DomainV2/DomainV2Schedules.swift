import Foundation

extension DomainV2 {
    public enum ScheduleFrequency: String, Codable, Equatable, Sendable {
        case daily
        case weekly
        case everyHours = "every_hours"
        case everyDays = "every_days"
        case everyMonths = "every_months"
        case once
    }

    public enum ScheduleStatus: String, Codable, Equatable, Sendable {
        case neverRun = "never_run"
        case launching
        case sent
        case failed
        case interrupted
    }

    /// A visible definition with the latest observed run projected by the Mac.
    public struct Schedule: Decodable, Equatable, Sendable, Identifiable {
        public let id: ScheduleId
        public let title: String
        public let text: String
        public let project: String?
        public let options: [String: OptionChoice]
        public let disallowedTools: [String]
        public let frequency: ScheduleFrequency
        public let hour: UInt8
        public let minute: UInt8
        public let weekday: UInt8?
        public let interval: UInt32?
        public let startAt: Int64?
        public let enabled: Bool
        public let nextRun: Int64?
        public let revision: Int64
        public let status: ScheduleStatus
        public let lastRun: Int64?
        public let lastRequest: RequestId?
        public let lastError: String?
        public let lastRunId: ScheduleRunId?

        enum CodingKeys: String, CodingKey {
            case id, title, text, project, options, frequency, hour, minute, weekday, interval
            case enabled, revision, status
            case disallowedTools = "disallowed_tools"
            case startAt = "start_at"
            case nextRun = "next_run"
            case lastRun = "last_run"
            case lastRequest = "last_request"
            case lastError = "last_error"
            case lastRunId = "last_run_id"
        }
    }

    /// `next_run` and run status are server-owned; they have no draft field.
    public struct ScheduleDraft: Encodable, Equatable, Sendable {
        public var title: String
        public var text: String
        public var project: String?
        public var options: [String: OptionChoice]
        public var disallowedTools: [String]
        public var frequency: ScheduleFrequency
        public var hour: UInt8
        public var minute: UInt8
        public var weekday: UInt8?
        public var interval: UInt32?
        public var startAt: Int64?
        public var enabled: Bool
        public var revision: Int64

        public init(title: String, text: String, project: String?, options: [String: OptionChoice] = [:],
                    disallowedTools: [String] = [], frequency: ScheduleFrequency, hour: UInt8,
                    minute: UInt8, weekday: UInt8? = nil, interval: UInt32? = nil,
                    startAt: Int64? = nil, enabled: Bool = true, revision: Int64 = 0) {
            self.title = title; self.text = text; self.project = project; self.options = options
            self.disallowedTools = disallowedTools; self.frequency = frequency
            self.hour = hour; self.minute = minute; self.weekday = weekday; self.interval = interval
            self.startAt = startAt; self.enabled = enabled; self.revision = revision
        }

        enum CodingKeys: String, CodingKey {
            case title, text, project, options, frequency, hour, minute, weekday, interval
            case enabled, revision
            case disallowedTools = "disallowed_tools"
            case startAt = "start_at"
        }
    }

    public struct ScheduleList: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let schedules: [Schedule]

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case dataEpoch = "data_epoch"
            case revision, schedules
        }
    }

    public struct ScheduleDetail: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let schedule: Schedule

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case dataEpoch = "data_epoch"
            case revision, schedule
        }
    }

    public struct ScheduleWriteReceipt: Decodable, Equatable, Sendable {
        public let id: ScheduleId
        public let revision: Int64
    }

    public struct ScheduleDeleteReceipt: Decodable, Equatable, Sendable {
        public let ok: Bool
    }

    /// A durable manual claim; Request/Task creation and Claude dispatch happen later.
    public struct ScheduleRunClaimReceipt: Decodable, Equatable, Sendable {
        public let ok: Bool
        public let runId: ScheduleRunId

        enum CodingKeys: String, CodingKey {
            case ok
            case runId = "run_id"
        }
    }
}
