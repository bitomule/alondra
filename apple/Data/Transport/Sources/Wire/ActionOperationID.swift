import CryptoKit
import Foundation

public struct ActionOperationID: Equatable, Sendable {
    public static let maximumCounter: UInt64 = 9_007_199_254_740_991
    private static let prefix = "op"
    public let epochTag: String
    public let generation: UInt64
    public let sequence: UInt64
    public let nonce: String

    public var rawValue: String { "\(Self.prefix)_\(epochTag)_\(generation)_\(sequence)_\(nonce)" }

    public init?(dataEpoch: String, generation: UInt64, sequence: UInt64, nonce: String) {
        guard !dataEpoch.isEmpty, (1...Self.maximumCounter).contains(generation),
              (1...Self.maximumCounter).contains(sequence), Self.validNonce(nonce) else { return nil }
        self.epochTag = Self.tag(for: dataEpoch)
        self.generation = generation; self.sequence = sequence; self.nonce = nonce
    }

    public init?(rawValue: String, dataEpoch: String? = nil) {
        guard rawValue.utf8.count <= 128 else { return nil }
        let fields = rawValue.split(separator: "_", omittingEmptySubsequences: false)
        guard fields.count == 5, fields[0] == Self.prefix,
              fields[1].utf8.count == 32,
              fields[1].utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              let generation = UInt64(fields[2]), String(generation) == fields[2],
              let sequence = UInt64(fields[3]), String(sequence) == fields[3],
              (1...Self.maximumCounter).contains(generation), (1...Self.maximumCounter).contains(sequence),
              Self.validNonce(String(fields[4])) else { return nil }
        epochTag = String(fields[1]); self.generation = generation; self.sequence = sequence; nonce = String(fields[4])
        if let dataEpoch, epochTag != Self.tag(for: dataEpoch) { return nil }
    }

    public static func tag(for dataEpoch: String) -> String {
        SHA256.hash(data: Data(dataEpoch.utf8)).prefix(16).map { String(format: "%02x", $0) }.joined()
    }

    public static func nonce(from commandID: String) -> String {
        ActionOperationID(rawValue: commandID)?.nonce ?? commandID
    }

    private static func validNonce(_ value: String) -> Bool {
        guard let uuid = UUID(uuidString: value), uuid.uuidString.lowercased() == value else { return false }
        return uuid.uuid.6 >> 4 == 4 && uuid.uuid.8 & 0xc0 == 0x80
    }
}

public struct ActionOperationRange: Codable, Equatable, Sendable {
    public let first: UInt64
    public let last: UInt64
    public init(first: UInt64, last: UInt64) { self.first = first; self.last = last }
}

public struct ActionJournalState: Decodable, Sendable {
    public let protocolVersion: Int
    public let dataEpoch: String
    public let generation: UInt64
    public let maxPending: Int
    public let maxRanges: Int
    public let maxSequence: UInt64
    public let acknowledged: Bool?
    public let ranges: [ActionOperationRange]?
    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol", dataEpoch = "data_epoch", generation, maxPending = "max_pending"
        case maxRanges = "max_ranges", maxSequence = "max_sequence", acknowledged, ranges
    }
}

public struct ActionJournalAcknowledgment: Encodable, Equatable, Sendable {
    public let dataEpoch: String
    public let generation: UInt64
    public let ranges: [ActionOperationRange]
    public init(dataEpoch: String, generation: UInt64, ranges: [ActionOperationRange]) {
        self.dataEpoch = dataEpoch; self.generation = generation; self.ranges = ranges
    }
    public func merging(_ prior: ActionJournalAcknowledgment?) throws -> ActionJournalAcknowledgment {
        guard generation > 0, generation <= ActionOperationID.maximumCounter, !dataEpoch.isEmpty,
              ranges.count <= 129,
              ranges.allSatisfy({ $0.first > 0 && $0.first <= $0.last && $0.last <= ActionOperationID.maximumCounter })
        else { throw OperationError.outcomeUnknown }
        if let prior, prior.generation > generation { return prior }
        if let prior, prior.generation == generation, prior.dataEpoch != dataEpoch { throw OperationError.outcomeUnknown }
        let combined = (ranges + (prior?.generation == generation ? prior!.ranges : [])).sorted { $0.first < $1.first }
        var merged: [ActionOperationRange] = []
        for range in combined {
            if let last = merged.last, range.first <= last.last + 1 {
                merged[merged.count - 1] = .init(first: last.first, last: max(last.last, range.last))
            } else { merged.append(range) }
        }
        guard merged.count <= 129 else { throw OperationError.outcomeUnknown }
        return .init(dataEpoch: dataEpoch, generation: generation, ranges: merged)
    }

    public func retires(_ operationID: String) -> Bool {
        guard let ticket = ActionOperationID(rawValue: operationID) else { return false }
        if ticket.generation < generation { return true }
        return ticket.generation == generation && ticket.epochTag == ActionOperationID.tag(for: dataEpoch)
            && ranges.contains { $0.first <= ticket.sequence && ticket.sequence <= $0.last }
    }
    enum CodingKeys: String, CodingKey { case dataEpoch = "data_epoch", generation, ranges }
}
