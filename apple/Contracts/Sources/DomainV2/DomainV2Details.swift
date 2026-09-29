import Foundation

extension DomainV2 {
    public struct PullRequestDiff: Codable, Equatable, Sendable {
        public let prId: PrId
        public let host: String
        public let repository: String
        public let number: UInt64
        public let head: String
        public let diff: String
        public let truncated: Bool
        public init(prId: PrId, host: String, repository: String, number: UInt64, head: String, diff: String, truncated: Bool) {
            self.prId = prId; self.host = host; self.repository = repository; self.number = number
            self.head = head; self.diff = diff; self.truncated = truncated
        }
        enum CodingKeys: String, CodingKey {
            case prId = "pr_id", host, repository, number, head, diff, truncated
        }
    }

    public struct DocumentContentChunk: Codable, Equatable, Sendable {
        public let revisionId: DocumentRevisionId
        public let contentDigest: String
        public let version: String
        public let data: String
        public let offset: UInt64
        public let total: UInt64
        public let nextOffset: UInt64
        public let eof: Bool
        enum CodingKeys: String, CodingKey {
            case revisionId = "revision_id", contentDigest = "content_digest", version, data
            case offset, total, nextOffset = "next_offset", eof
        }
    }

    public struct TaskActions: Codable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let actions: [AvailableAction]
        public init(protocolVersion: UInt32, dataEpoch: DataEpoch, revision: Revision, actions: [AvailableAction]) {
            self.protocolVersion = protocolVersion; self.dataEpoch = dataEpoch; self.revision = revision; self.actions = actions
        }
        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision, actions
        }
    }
}
