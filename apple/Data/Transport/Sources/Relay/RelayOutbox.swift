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
    public var envelope: EnvelopeV1?
    public var phase: Phase
    public var lastStatus: String?
    public var lastRevision: UInt64?
    public var lastResponse: EnvelopeV1?
    /// Keyed request binding; contains no plaintext and survives retirement.
    public var semanticTag: Data? = nil

}

/// The encrypted envelopes this process sent, by request ID, so a lost response is
/// reconciled by status instead of POSTed again. Nothing is persisted: a v2 command keeps its
/// command ID in the draft and a retry after relaunch is a new request the server deduplicates.
public actor RelayOutbox {
    private let scope: RelayOutboxScope
    private let transport: any RelayRequestTransport
    private var records: [String: RelayOutboxEntry] = [:]

    public init(scope: RelayOutboxScope, transport: any RelayRequestTransport) {
        self.scope = scope
        self.transport = transport
    }

    public func entry(requestID: String) throws -> RelayOutboxEntry? {
        records[requestID]
    }

    /// The caller must seal a fresh envelope before this call. A record is durable before POST.
    /// Network errors and post-send disk errors are both uncertain; neither authorizes retry.
    public func submitNew(_ envelope: EnvelopeV1, semanticTag: Data? = nil) async throws -> RelaySubmission {
        _ = try envelope.signedBytes()
        guard envelope.kind == "request" else { throw RelayOutboxError.invalidEnvelope }
        guard try JSONEncoder().encode(envelope).count <= 256 * 1024 else {
            throw RelayOutboxError.invalidEnvelope
        }
        guard envelope.route == scope.macID, envelope.recipient == scope.macID,
              envelope.sender == scope.deviceID else { throw RelayOutboxError.wrongScope }
        guard records[envelope.requestID] == nil else { throw RelayOutboxError.duplicateRequestID }
        records[envelope.requestID] = RelayOutboxEntry(
            requestID: envelope.requestID, scope: scope, envelope: envelope, phase: .reserved, semanticTag: semanticTag)

        do {
            let receipt = try await transport.submitRequest(envelope)
            guard receipt.requestID == envelope.requestID else {
                try? setUncertain(envelope.requestID)
                throw RelayOutboxError.transportResponseMismatch
            }
            do {
                let saved = try update(envelope.requestID, phase: .acknowledged, receipt: receipt)
                return .accepted(Self.receipt(from: saved) ?? receipt)
            } catch {
                try? setUncertain(envelope.requestID)
                return .uncertain
            }
        } catch RelayOutboxError.transportResponseMismatch {
            return .uncertain
        } catch {
            try? setUncertain(envelope.requestID)
            return .uncertain
        }
    }

    /// Read-only reconciliation. A missing/failed status lookup leaves the entry uncertain.
    public func reconcile(requestID: String) async throws -> RelayReceipt? {
        guard let record = records[requestID], record.phase != .retired else { return nil }
        let receipt = try await transport.requestStatus(requestID: requestID)
        guard receipt.requestID == requestID else { throw RelayOutboxError.transportResponseMismatch }
        let saved = try update(requestID, phase: .acknowledged, receipt: receipt)
        guard saved.phase != .retired else { return nil }
        return Self.receipt(from: saved) ?? receipt
    }

    /// Call only after the verified completed response has been durably applied to app state.
    /// Keeps a small request-ID tombstone so the same ID can never be POSTed again.
    public func retireAppliedResponse(requestID: String, revision: UInt64,
                                      verifiedResponse: EnvelopeV1) throws {
        guard var record = records[requestID] else { throw RelayOutboxError.persistenceFailed }
        guard record.phase == .acknowledged, record.lastStatus == "completed",
              record.lastRevision == revision, record.lastResponse == verifiedResponse
        else { throw RelayOutboxError.resultNotApplied }
        record.phase = .retired
        record.envelope = nil
        record.lastResponse = nil
        records[requestID] = record
    }

    private func setUncertain(_ id: String) throws {
        _ = try update(id, phase: .uncertain, receipt: nil)
    }

    private static func receipt(from record: RelayOutboxEntry) -> RelayReceipt? {
        guard let status = record.lastStatus, let revision = record.lastRevision else { return nil }
        return RelayReceipt(requestID: record.requestID, status: status,
                            revision: revision, response: record.lastResponse)
    }

    @discardableResult
    private func update(_ id: String, phase: RelayOutboxEntry.Phase,
                        receipt: RelayReceipt?) throws -> RelayOutboxEntry {
        guard var record = records[id] else { throw RelayOutboxError.persistenceFailed }
        if record.phase == .retired { return record }
        if receipt == nil && record.phase == .acknowledged { return record }
        if let receipt, let lastRevision = record.lastRevision, receipt.revision < lastRevision {
            return record // A delayed read must not roll back a newer observed result.
        }
        record.phase = phase
        if let receipt {
            record.lastStatus = receipt.status
            record.lastRevision = receipt.revision
            record.lastResponse = receipt.response ?? record.lastResponse
        }
        records[id] = record
        return record
    }
}
