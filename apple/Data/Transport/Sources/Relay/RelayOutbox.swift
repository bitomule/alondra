import Darwin
import Foundation

public enum RelayOutboxError: Error, Equatable, Sendable {
    case duplicateRequestID
    case invalidEnvelope
    case transportResponseMismatch
    case persistenceFailed
    case wrongScope
    case resultNotApplied
}

public enum RelaySubmission: Sendable {
    case accepted(RelayReceipt)
    /// The POST may have reached the relay. Only GET status may resolve it.
    case uncertain
}

public struct RelayOutboxScope: Codable, Sendable, Equatable {
    public let relay: URL
    public let macID: String
    public let deviceID: String
    public let credentialRevision: String

    public init(relay: URL, macID: String, deviceID: String, credentialRevision: String) {
        self.relay = relay
        self.macID = macID
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
    }
}

public struct RelayOutboxEntry: Codable, Sendable, Equatable {
    public enum Phase: String, Codable, Sendable { case reserved, uncertain, acknowledged, retired }

    public let requestID: String
    public let scope: RelayOutboxScope
    public var envelope: Envelope?
    public var phase: Phase
    public var lastStatus: String?
    public var lastRevision: UInt64?
    public var lastResponse: Envelope?
    /// Keyed request binding; contains no plaintext and survives retirement.
    public var semanticTag: Data? = nil

}

/// Encrypted requests reserved before dispatch, reconciled without replay after an ambiguous send.
public actor RelayOutbox {
    private let scope: RelayOutboxScope
    private let transport: any RelayRequestTransport
    private let journal: EnvelopeJournal?
    private var records: [String: RelayOutboxEntry] = [:]
    private var retirement: ActionJournalAcknowledgment?

    public init(scope: RelayOutboxScope, transport: any RelayRequestTransport, journal: EnvelopeJournal? = nil) {
        self.scope = scope
        self.transport = transport
        self.journal = journal
    }

    public func entry(requestID: String) async throws -> RelayOutboxEntry? {
        if let journal { return try await journal.entry(requestID: requestID) }
        if retirement?.retires(requestID) == true {
            return RelayOutboxEntry(requestID: requestID, scope: scope, envelope: nil, phase: .retired)
        }
        return records[requestID]
    }

    /// A session record precedes POST. Network failures stay uncertain and never authorize replay.
    public func submitNew(_ envelope: Envelope, semanticTag: Data? = nil,
                          reserved: @Sendable () -> Void = {}) async throws -> RelaySubmission {
        _ = try envelope.signedBytes()
        guard envelope.kind == "request" else { throw RelayOutboxError.invalidEnvelope }
        guard try JSONEncoder().encode(envelope).count <= 256 * 1024 else {
            throw RelayOutboxError.invalidEnvelope
        }
        guard envelope.route == scope.macID, envelope.recipient == scope.macID,
              envelope.sender == scope.deviceID else { throw RelayOutboxError.wrongScope }
        guard try await entry(requestID: envelope.requestID) == nil else { throw RelayOutboxError.duplicateRequestID }
        try MutationScope.check()
        let record = RelayOutboxEntry(
            requestID: envelope.requestID, scope: scope, envelope: envelope, phase: .reserved, semanticTag: semanticTag)
        if let journal {
            do { try await journal.reserve(record) }
            catch EnvelopeJournalError.duplicateRequestID { throw RelayOutboxError.duplicateRequestID }
        }
        else { records[envelope.requestID] = record }
        reserved()

        do {
            try MutationScope.check()
            let receipt = try await transport.submitRequest(envelope)
            guard receipt.requestID == envelope.requestID else {
                try? await setUncertain(envelope.requestID)
                throw RelayOutboxError.transportResponseMismatch
            }
            do {
                let saved = try await update(envelope.requestID, phase: .acknowledged, receipt: receipt)
                return .accepted(Self.receipt(from: saved) ?? receipt)
            } catch {
                try? await setUncertain(envelope.requestID)
                return .uncertain
            }
        } catch RelayOutboxError.transportResponseMismatch {
            return .uncertain
        } catch {
            try? await setUncertain(envelope.requestID)
            return .uncertain
        }
    }

    /// Read-only reconciliation. A missing/failed status lookup leaves the entry uncertain.
    public func reconcile(requestID: String) async throws -> RelayReceipt? {
        guard let record = try await entry(requestID: requestID), record.phase != .retired else { return nil }
        let receipt = try await transport.requestStatus(requestID: requestID)
        guard receipt.requestID == requestID else { throw RelayOutboxError.transportResponseMismatch }
        let saved = try await update(requestID, phase: .acknowledged, receipt: receipt)
        guard saved.phase != .retired else { return nil }
        return Self.receipt(from: saved) ?? receipt
    }

    /// Call only after the verified completed response has been durably applied to app state.
    /// Keeps a small request-ID tombstone so the same ID can never be POSTed again.
    public func retireAppliedResponse(requestID: String, revision: UInt64,
                                      verifiedResponse: Envelope) async throws {
        let retire: @Sendable (inout RelayOutboxEntry) throws -> Void = { record in
            guard record.phase == .acknowledged, record.lastStatus == "completed",
                  record.lastRevision == revision, record.lastResponse == verifiedResponse
            else { throw RelayOutboxError.resultNotApplied }
            record.phase = .retired
            record.envelope = nil
            record.lastResponse = nil
        }
        if let journal { _ = try await journal.update(requestID: requestID, retire) }
        else {
            guard var record = records[requestID] else { throw RelayOutboxError.persistenceFailed }
            try retire(&record)
            records[requestID] = record
        }
    }

    public func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        try await journal?.retire(acknowledgment)
        let merged = try acknowledgment.merging(retirement)
        retirement = merged
        records = records.filter { !merged.retires($0.key) }
    }

    private func setUncertain(_ id: String) async throws {
        _ = try await update(id, phase: .uncertain, receipt: nil)
    }

    private static func receipt(from record: RelayOutboxEntry) -> RelayReceipt? {
        guard let status = record.lastStatus, let revision = record.lastRevision else { return nil }
        return RelayReceipt(requestID: record.requestID, status: status,
                            revision: revision, response: record.lastResponse)
    }

    @discardableResult
    private func update(_ id: String, phase: RelayOutboxEntry.Phase,
                        receipt: RelayReceipt?) async throws -> RelayOutboxEntry {
        let change: @Sendable (inout RelayOutboxEntry) -> Void = { record in
            if record.phase == .retired { return }
            if receipt == nil && record.phase == .acknowledged { return }
            if let receipt, let lastRevision = record.lastRevision, receipt.revision < lastRevision {
                return // A delayed read must not roll back a newer observed result.
            }
            record.phase = phase
            if let receipt {
                record.lastStatus = receipt.status
                record.lastRevision = receipt.revision
                record.lastResponse = receipt.response ?? record.lastResponse
            }
        }
        if let journal { return try await journal.update(requestID: id, change) }
        guard var record = records[id] else {
            if retirement?.retires(id) == true {
                return RelayOutboxEntry(requestID: id, scope: scope, envelope: nil, phase: .retired)
            }
            throw RelayOutboxError.persistenceFailed
        }
        change(&record)
        records[id] = record
        return record
    }
}
