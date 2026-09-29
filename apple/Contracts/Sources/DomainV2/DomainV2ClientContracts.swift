import Foundation

extension DomainV2 {
    /// One slice of a stored file, `GET /files/chunk?path=&offset=&version=`.
    public struct AttachmentContentChunk: Codable, Equatable, Sendable {
        public let contentDigest: String
        public let version: String
        public let data: String
        public let offset: UInt64
        public let total: UInt64
        public let nextOffset: UInt64
        public let eof: Bool
        enum CodingKeys: String, CodingKey {
            case contentDigest = "content_digest", version, data
            case offset, total, nextOffset = "next_offset", eof
        }
    }
}
