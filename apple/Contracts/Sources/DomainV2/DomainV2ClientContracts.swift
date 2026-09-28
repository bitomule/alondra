import Foundation

/// The GitHub observation of a PR, in the shape the apps already render.
public typealias RemotePullRequest = PullRequest

/// Reads the apps need that the v2 daemon does not serve yet. Each mirrors the body of the v1
/// route it replaces so the daemon side can reuse that serializer; see
/// docs/domain-v2-apple-client-contracts.md for the exact routes.
extension DomainV2 {
    /// Technical Agent telemetry, machine and usage: never business state (R10).
    /// Decoded with snake_case conversion, like the v1 overview it replaces.
    public struct Runtime: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let machine: Machine?
        public let usage: Usage?
        public let nodes: [Node]
        /// What GitHub last reported for each registered PR, keyed by the domain PR ID. It
        /// describes the remote PR; it never decides merge authorization or completion.
        public let pullRequests: [PullRequestObservation]?
        /// Task history for display; the Task state itself is the projection's.
        public let tasks: [TaskObservation]?
        public let requests: [RequestObservation]?
        public let interactions: [InteractionObservation]?

        public init(protocolVersion: UInt32, dataEpoch: DataEpoch, machine: Machine?, usage: Usage?, nodes: [Node],
                    pullRequests: [PullRequestObservation]? = nil, tasks: [TaskObservation]? = nil,
                    requests: [RequestObservation]? = nil, interactions: [InteractionObservation]? = nil) {
            self.protocolVersion = protocolVersion
            self.dataEpoch = dataEpoch
            self.machine = machine
            self.usage = usage
            self.nodes = nodes
            self.pullRequests = pullRequests
            self.tasks = tasks
            self.requests = requests
            self.interactions = interactions
        }

        public func request(_ id: RequestId?) -> RequestObservation? {
            id.flatMap { id in requests?.first { $0.requestId == id } }
        }

        public func createdAt(_ id: InteractionId?) -> Int64? {
            id.flatMap { id in interactions?.first { $0.interactionId == id }?.createdAt }
        }

        public func node(_ agent: AgentId?) -> Node? {
            agent.flatMap { id in nodes.first { $0.sessionId == id.rawValue } }
        }

        public func pullRequest(_ id: PrId?) -> RemotePullRequest? {
            id.flatMap { id in pullRequests?.first { $0.prId == id }?.observation }
        }

        public func task(_ id: TaskId?) -> TaskObservation? {
            id.flatMap { id in tasks?.first { $0.taskId == id } }
        }
    }

    public struct PullRequestObservation: Decodable, Equatable, Sendable {
        public let prId: PrId
        public let observation: RemotePullRequest
    }

    public struct TaskObservation: Decodable, Equatable, Sendable {
        public let taskId: TaskId
        public let createdAt: Int64?
        public let stateSince: Int64?
        public let project: String?
        public let effort: String?
        public let issue: String?
        public let history: [TaskTransition]?
    }

    public struct RequestObservation: Decodable, Equatable, Sendable {
        public let requestId: RequestId
        public let title: String?
        public let createdAt: Int64?
        public let project: String?
        public let effort: String?
        public let issue: String?
    }

    public struct InteractionObservation: Decodable, Equatable, Sendable {
        public let interactionId: InteractionId
        public let createdAt: Int64?
    }

    /// One recorded change of a Task's state and what caused it, for the detail's history.
    public struct TaskTransition: Decodable, Hashable, Sendable {
        public let from: TaskState?
        public let to: TaskState
        public let cause: String
        public let blockerKind: String?
        public let at: Int64
    }

    public struct AttachmentContentChunk: Codable, Equatable, Sendable {
        public let attachmentId: String
        public let contentDigest: String
        public let version: String
        public let data: String
        public let offset: UInt64
        public let total: UInt64
        public let nextOffset: UInt64
        public let eof: Bool
        enum CodingKeys: String, CodingKey {
            case attachmentId = "attachment_id", contentDigest = "content_digest", version, data
            case offset, total, nextOffset = "next_offset", eof
        }
    }
}
