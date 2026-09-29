import Foundation

/// Sectioned reads (docs/domain-v2-read-endpoints.md). Rows are the domain DTOs; these
/// envelopes add paging, per-section tokens and the truncation signal.
extension DomainV2 {
    public enum Section: String, CaseIterable, Sendable {
        case inbox, tasks, requests, agents, interactions
        case pullRequests = "pull_requests"
        case documents
        case mergeAuthorizations = "merge_authorizations"
        case stackReadiness = "stack_readiness"
        case supersededAcceptances = "superseded_acceptances"
        case schedules
    }

    public struct SectionInfo: Codable, Equatable, Sendable {
        public let revision: String
        public let count: UInt64
    }

    public struct Summary: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let counts: InboxCounts
        public let openTasks: UInt64
        public let connectedAgents: UInt64
        public let sections: [String: SectionInfo]
        public let pending: [String]

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision, counts
            case openTasks = "open_tasks", connectedAgents = "connected_agents", sections, pending
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            protocolVersion = try container.decode(UInt32.self, forKey: .protocolVersion)
            dataEpoch = try container.decode(DataEpoch.self, forKey: .dataEpoch)
            revision = try container.decode(Revision.self, forKey: .revision)
            counts = try container.decode(InboxCounts.self, forKey: .counts)
            openTasks = try container.decode(UInt64.self, forKey: .openTasks)
            connectedAgents = try container.decode(UInt64.self, forKey: .connectedAgents)
            sections = try container.decode([String: SectionInfo].self, forKey: .sections)
            pending = try container.decodeIfPresent([String].self, forKey: .pending) ?? []
        }
    }

    public struct Page<Row: Decodable & Sendable & Equatable>: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let section: String
        public let sectionRevision: String
        public let total: UInt64
        public let items: [Row]
        public let nextCursor: String?
        public let truncated: [String]

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision, section
            case sectionRevision = "section_revision", total, items, nextCursor = "next_cursor", truncated
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            protocolVersion = try container.decode(UInt32.self, forKey: .protocolVersion)
            dataEpoch = try container.decode(DataEpoch.self, forKey: .dataEpoch)
            revision = try container.decode(Revision.self, forKey: .revision)
            section = try container.decode(String.self, forKey: .section)
            sectionRevision = try container.decode(String.self, forKey: .sectionRevision)
            total = try container.decode(UInt64.self, forKey: .total)
            items = try container.decode([Row].self, forKey: .items)
            nextCursor = try container.decodeIfPresent(String.self, forKey: .nextCursor)
            truncated = try container.decodeIfPresent([String].self, forKey: .truncated) ?? []
        }
    }

    public struct Item<Row: Decodable & Sendable & Equatable>: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public var item: Row
        public let truncated: Bool

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision, item, truncated
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            protocolVersion = try container.decode(UInt32.self, forKey: .protocolVersion)
            dataEpoch = try container.decode(DataEpoch.self, forKey: .dataEpoch)
            revision = try container.decode(Revision.self, forKey: .revision)
            item = try container.decode(Row.self, forKey: .item)
            truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        }
    }

    public struct InboxLookup: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let item: InboxItem
        public let rootId: String

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision, item, rootId = "root_id"
        }
    }

    public struct DocumentBundle: Codable, Equatable, Sendable {
        public let document: Document
        public let revisions: [DocumentRevision]
        public let reviews: [DocumentReview]
    }

    public struct TaskDetail: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let task: Task
        public let request: Request
        public let agent: Agent?
        public let pullRequest: PullRequest?
        public let documents: [DocumentBundle]
        public let mergeAuthorizations: [MergeAuthorization]
        public let stackReadiness: StackReadiness?
        public let supersededAcceptance: SupersededAcceptance?
        public let actions: [AvailableAction]
        public let interactions: UInt64

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision, task, request, agent
            case pullRequest = "pull_request", documents, mergeAuthorizations = "merge_authorizations"
            case stackReadiness = "stack_readiness", supersededAcceptance = "superseded_acceptance", actions, interactions
        }
    }

    public struct PullRequestDetail: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let pullRequest: PullRequest
        public let mergeAuthorizations: [MergeAuthorization]
        public let stackReadiness: [StackReadiness]

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision
            case pullRequest = "pull_request", mergeAuthorizations = "merge_authorizations", stackReadiness = "stack_readiness"
        }
    }

    public struct TextChunk: Decodable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision
        public let id: String
        public let offset: UInt64
        public let totalBytes: UInt64
        public let data: String
        public let hasMore: Bool

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version", dataEpoch = "data_epoch", revision, id, offset
            case totalBytes = "total_bytes", data, hasMore = "has_more"
        }
    }
}
