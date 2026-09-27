import Foundation

extension DomainV2.DomainError: Error {}

extension DomainV2 {
    /// The domain wire uses explicit CodingKeys. Do not apply convertFromSnakeCase
    /// to these DTOs: that would change keys before the declared mapping runs.
    public static func decode<Value: Decodable>(_ type: Value.Type, from data: Data) throws -> Value {
        try JSONDecoder().decode(type, from: data)
    }

    public static func encode<Value: Encodable>(_ value: Value) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(value)
    }
}

extension DomainV2 {
    public struct Capabilities: Codable, Equatable, Sendable {
        public let protocolVersion: UInt32
        public let dataEpoch: DataEpoch
        public let revision: Revision

        public init(protocolVersion: UInt32, dataEpoch: DataEpoch, revision: Revision) {
            self.protocolVersion = protocolVersion
            self.dataEpoch = dataEpoch
            self.revision = revision
        }

        enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case dataEpoch = "data_epoch"
            case revision
        }
    }
}
