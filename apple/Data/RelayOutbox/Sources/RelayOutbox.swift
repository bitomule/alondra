import AlondraRelayClient
import Darwin
import Foundation

public struct RelayReceipt: Sendable {
    public let requestID: String
    public let status: String
    public let revision: UInt64
    public let response: EnvelopeV1?

    public init(requestID: String, status: String, revision: UInt64, response: EnvelopeV1?) {
        self.requestID = requestID
        self.status = status
        self.revision = revision
        self.response = response
    }
}

public protocol RelayRequestTransport: Sendable {
    func submitRequest(_ envelope: EnvelopeV1) async throws -> RelayReceipt
    func requestStatus(requestID: String) async throws -> RelayReceipt
}

extension AlondraRelayClient: RelayRequestTransport {
    public func submitRequest(_ envelope: EnvelopeV1) async throws -> RelayReceipt {
        let job: RelayJob = try await submit(envelope)
        return RelayReceipt(requestID: job.requestID, status: job.status,
                            revision: job.revision, response: job.response)
    }

    public func requestStatus(requestID: String) async throws -> RelayReceipt {
        let job: RelayJob = try await status(requestID: requestID)
        return RelayReceipt(requestID: job.requestID, status: job.status,
                            revision: job.revision, response: job.response)
    }
}

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

}

/// Stores only an encrypted envelope. The exact request ID and envelope survive a crash before
/// or during POST. Existing entries are never automatically POSTed again, even after a 404.
public actor RelayOutbox {
    private let file: URL
    private let scope: RelayOutboxScope
    private let transport: any RelayRequestTransport

    public init(file: URL, scope: RelayOutboxScope, transport: any RelayRequestTransport) {
        self.file = file
        self.scope = scope
        self.transport = transport
    }

    public func entry(requestID: String) throws -> RelayOutboxEntry? {
        guard let record = try withLock({ try readFile() })[requestID] else { return nil }
        guard record.scope == scope else { throw RelayOutboxError.wrongScope }
        return record
    }

    /// The caller must seal a fresh envelope before this call. A record is durable before POST.
    /// Network errors and post-send disk errors are both uncertain; neither authorizes retry.
    public func submitNew(_ envelope: EnvelopeV1) async throws -> RelaySubmission {
        _ = try envelope.signedBytes()
        guard envelope.kind == "request" else { throw RelayOutboxError.invalidEnvelope }
        guard try JSONEncoder().encode(envelope).count <= 256 * 1024 else {
            throw RelayOutboxError.invalidEnvelope
        }
        guard envelope.route == scope.macID, envelope.recipient == scope.macID,
              envelope.sender == scope.deviceID else { throw RelayOutboxError.wrongScope }
        try withLock {
            var records = try readFile()
            guard records[envelope.requestID] == nil else { throw RelayOutboxError.duplicateRequestID }
            records[envelope.requestID] = RelayOutboxEntry(
                requestID: envelope.requestID, scope: scope, envelope: envelope, phase: .reserved)
            try writeFile(records)
        }

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
        guard let record = try withLock({ try readFile() })[requestID] else { return nil }
        guard record.scope == scope else { throw RelayOutboxError.wrongScope }
        guard record.phase != .retired else { return nil }
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
        try withLock {
            var records = try readFile()
            guard var record = records[requestID] else { throw RelayOutboxError.persistenceFailed }
            guard record.scope == scope else { throw RelayOutboxError.wrongScope }
            guard record.phase == .acknowledged, record.lastStatus == "completed",
                  record.lastRevision == revision, record.lastResponse == verifiedResponse
            else { throw RelayOutboxError.resultNotApplied }
            record.phase = .retired
            record.envelope = nil
            record.lastResponse = nil
            records[requestID] = record
            try writeFile(records)
        }
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
        try withLock {
            var records = try readFile()
            guard var record = records[id] else { throw RelayOutboxError.persistenceFailed }
            guard record.scope == scope else { throw RelayOutboxError.wrongScope }
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
            try writeFile(records)
            return record
        }
    }

    private func withLock<T>(_ body: () throws -> T) throws -> T {
        do {
            try FileManager.default.createDirectory(at: file.deletingLastPathComponent(),
                                                    withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
        } catch { throw RelayOutboxError.persistenceFailed }
        let descriptor = open(file.path + ".lock", O_CREAT | O_RDWR, 0o600)
        guard descriptor >= 0 else { throw RelayOutboxError.persistenceFailed }
        defer { close(descriptor) }
        guard flock(descriptor, LOCK_EX) == 0 else { throw RelayOutboxError.persistenceFailed }
        defer { flock(descriptor, LOCK_UN) }
        return try body()
    }

    private func readFile() throws -> [String: RelayOutboxEntry] {
        do {
            let data = try Data(contentsOf: file)
            let decoded = try JSONDecoder().decode([String: RelayOutboxEntry].self, from: data)
            guard decoded.allSatisfy({ $0.key == $0.value.requestID }) else {
                throw RelayOutboxError.persistenceFailed
            }
            return decoded
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return [:]
        } catch {
            throw RelayOutboxError.persistenceFailed
        }
    }

    private func writeFile(_ records: [String: RelayOutboxEntry]) throws {
        do {
            let data = try JSONEncoder().encode(records)
            try data.write(to: file, options: [.atomic, .completeFileProtectionUnlessOpen])
        } catch {
            throw RelayOutboxError.persistenceFailed
        }
    }
}
