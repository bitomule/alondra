import AlondraContract
import AlondraTransport
import CryptoKit
import Foundation

public enum ViewActionIntentError: Error, Equatable, Sendable {
    case invalidScope, invalidRecord, unreadableStore, capacityExceeded
}

public struct ActionIntentScope: Codable, Equatable, Sendable {
    public let service: String
    public let device: String
    public let credentialRevision: String
    public init(service: String, device: String, credentialRevision: String) {
        self.service = service; self.device = device; self.credentialRevision = credentialRevision
    }
}

public actor ViewActionIntentStore {
    public enum Phase: String, Codable, Sendable { case prepared, dispatched }
    public struct Record: Codable, Sendable {
        public let request: ActionRequest
        public let body: Data
        public let resolvesItsCard: Bool
        public let phase: Phase
        init(request: ActionRequest, body: Data, phase: Phase = .dispatched) {
            self.request = request; self.body = body; resolvesItsCard = request.resolvesItsCard; self.phase = phase
        }
        private enum CodingKeys: String, CodingKey { case request, body, resolvesItsCard, phase }
        public init(from decoder: any Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            request = try values.decode(ActionRequest.self, forKey: .request)
            body = try values.decode(Data.self, forKey: .body)
            resolvesItsCard = try values.decode(Bool.self, forKey: .resolvesItsCard)
            phase = try values.decodeIfPresent(Phase.self, forKey: .phase) ?? .dispatched
        }
        public var restoredRequest: ActionRequest {
            var value = request; value.resolvesItsCard = resolvesItsCard; return value
        }
    }
    struct Completed: Codable, Sendable {
        let commandID: String
        let bodyDigest: Data
        let dataEpoch: String
        var response: ActionResponse?
    }
    enum Reservation: Sendable { case created(Record), pending(Record), completed(ActionResponse?) }
    struct Snapshot: Codable, Sendable {
        var pending: [Record] = []
        var completed: [Completed] = []
    }
    public struct RetiredInterval: Codable, Sendable, Equatable {
        public var first: UInt64
        public var last: UInt64
    }
    public struct Acknowledgments: Sendable {
        public let generation: UInt64
        public let dataEpoch: String
        public let ranges: [RetiredInterval]
    }
    public struct Statistics: Sendable {
        public let pending: Int
        public let recent: Int
        public let legacy: Int
        public let retiredIntervals: Int
        public let encryptedBytes: Int
        public let databaseBytes: Int
    }
    static let maxPending = 128
    static let maxCompleted = 16_384
    static let maxBytes = 4 * 1024 * 1024
    public nonisolated let restored: [Record]
    private let database: ActionIntentDatabase
    private let allowLegacyDispatch: Bool

    public init(directory: URL, keyMaterial: Data, scope: ActionIntentScope, allowLegacyDispatch: Bool = false) async throws {
        try await self.init(directory: directory, keyMaterial: keyMaterial, scope: scope,
            completedLimit: Self.maxCompleted, allowLegacyDispatch: allowLegacyDispatch)
    }
    init(directory: URL, keyMaterial: Data, scope: ActionIntentScope, completedLimit: Int, allowLegacyDispatch: Bool = false) async throws {
        let database = try ActionIntentDatabase(directory: directory, keyMaterial: keyMaterial,
            scope: scope, legacyLimit: completedLimit)
        restored = try await database.perform { try database.pending($0) }
        self.database = database; self.allowLegacyDispatch = allowLegacyDispatch
    }
    public func prepareNew(_ request: ActionRequest, generation: UInt64) async throws -> Record {
        let database = database
        return try await database.perform { try database.prepareNew($0, request: request, generation: generation) }
    }
    func reserve(_ request: ActionRequest, body: Data, allowLegacyDispatch override: Bool? = nil) async throws -> Reservation {
        let database = database; let legacy = override ?? allowLegacyDispatch
        return try await database.perform { try database.reserve($0, request: request, body: body, allowLegacy: legacy) }
    }
    func complete(_ record: Record, response: ActionResponse) async throws {
        let database = database
        try await database.perform { try database.complete($0, record: record, response: response) }
    }
    func reissue(_ record: Record) async throws -> Record {
        let database = database
        return try await database.perform { try database.reissue($0, record: record) }
    }
    func discard(_ record: Record) async throws {
        let database = database
        try await database.perform { try database.discard($0, record: record) }
    }
    func markAcknowledged(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        let database = database
        try await database.perform { try database.markAcknowledged($0, acknowledgment: acknowledgment) }
    }
    public func acknowledgmentRanges() async throws -> Acknowledgments {
        let database = database
        return try await database.perform { try database.acknowledgments($0) }
    }
    public func statistics() async throws -> Statistics {
        let database = database
        return try await database.perform { try database.statistics($0) }
    }
    static func sameIntent(_ lhs: ActionRequest, _ rhs: ActionRequest) -> Bool {
        lhs.protocolVersion == rhs.protocolVersion && lhs.dataEpoch == rhs.dataEpoch
            && lhs.actionId == rhs.actionId && lhs.optionId == rhs.optionId && lhs.input == rhs.input
    }
    static func validate(_ record: Record) throws {
        guard !record.request.commandId.isEmpty, !record.request.dataEpoch.isEmpty,
              record.body.count <= 256 * 1024, try ContractCodec.encode(record.restoredRequest) == record.body
        else { throw ViewActionIntentError.invalidRecord }
    }
}
