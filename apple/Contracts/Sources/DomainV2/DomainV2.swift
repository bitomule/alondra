import Foundation

/// Authoritative domain v2 wire values. Business rules and available actions belong to the server.
public enum DomainV2 {
    public static let protocolVersion: UInt32 = 2
    public static let maximumRevision: UInt64 = 9_007_199_254_740_991
    public typealias Revision = UInt64

    public struct ID<Tag: Sendable>: RawRepresentable, Hashable, Codable, Sendable, ExpressibleByStringLiteral {
        public let rawValue: String
        public init(rawValue: String) { self.rawValue = rawValue }
        public init(stringLiteral value: String) { self.rawValue = value }
        public init(from decoder: any Decoder) throws {
            let container = try decoder.singleValueContainer()
            rawValue = try container.decode(String.self)
            guard !rawValue.isEmpty else { throw DecodingError.dataCorruptedError(in: container, debugDescription: "Empty domain ID") }
        }
        public func encode(to encoder: any Encoder) throws {
            var container = encoder.singleValueContainer()
            try container.encode(rawValue)
        }
    }
    public enum TaskIdTag: Sendable {}
    public typealias TaskId = ID<TaskIdTag>
    public enum AgentIdTag: Sendable {}
    public typealias AgentId = ID<AgentIdTag>
    public enum InteractionIdTag: Sendable {}
    public typealias InteractionId = ID<InteractionIdTag>
    public enum DocumentIdTag: Sendable {}
    public typealias DocumentId = ID<DocumentIdTag>
    public enum DocumentRevisionIdTag: Sendable {}
    public typealias DocumentRevisionId = ID<DocumentRevisionIdTag>
    public enum PrIdTag: Sendable {}
    public typealias PrId = ID<PrIdTag>
    public enum RequestIdTag: Sendable {}
    public typealias RequestId = ID<RequestIdTag>
    public enum AuthorizationIdTag: Sendable {}
    public typealias AuthorizationId = ID<AuthorizationIdTag>
    public enum CommandIdTag: Sendable {}
    public typealias CommandId = ID<CommandIdTag>
    public enum DataEpochTag: Sendable {}
    public typealias DataEpoch = ID<DataEpochTag>
    public enum ScheduleIdTag: Sendable {}
    public typealias ScheduleId = ID<ScheduleIdTag>
    public enum ScheduleRunIdTag: Sendable {}
    public typealias ScheduleRunId = ID<ScheduleRunIdTag>
}
