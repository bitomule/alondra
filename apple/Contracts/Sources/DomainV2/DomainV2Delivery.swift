import Foundation

extension DomainV2 {
    /// Durable server-to-recipient delivery evidence. These IDs are distinct from
    /// client command UUIDs and must not clear the client's uncertain command journal.
    public struct DeliveryAttempt: Codable, Equatable, Sendable, Identifiable {
        public let attemptId: Int64
        public let messageId: Int64
        public let state: String
        public let currentState: String
        public let version: UInt64
        public let reason: String?
        public let recipientKind: String
        public let taskId: TaskId?
        public let interactionId: InteractionId?
        public var id: Int64 { attemptId }

        enum CodingKeys: String, CodingKey {
            case attemptId = "attempt_id", messageId = "message_id", state
            case currentState = "current_state", version, reason
            case recipientKind = "recipient_kind", taskId = "task_id"
            case interactionId = "interaction_id"
        }
    }

    public struct DeliveryAttemptPage: Codable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let attempts: [DeliveryAttempt]
        public let nextAfter: Int64
        public let hasMore: Bool

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision
            case attempts, nextAfter = "next_after", hasMore = "has_more"
        }
    }

    public enum DeliveryDecision: String, Codable, Equatable, Sendable {
        case retry, discard
    }

    public struct ResolveDeliveryAttempt: Codable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let expectedVersion: UInt64
        public let decision: DeliveryDecision

        public init(dataEpoch: DataEpoch, expectedVersion: UInt64, decision: DeliveryDecision) {
            protocolVersion = DomainV2.protocolVersion
            self.dataEpoch = dataEpoch
            self.expectedVersion = expectedVersion
            self.decision = decision
        }

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch"
            case expectedVersion = "expected_version", decision
        }
    }

    public struct DeliveryResolution: Codable, Equatable, Sendable {
        public let attemptId: Int64
        public let messageId: Int64
        public let version: UInt64
        public let state: String

        enum CodingKeys: String, CodingKey {
            case attemptId = "attempt_id", messageId = "message_id", version, state
        }
    }
}
