import AlondraAPIClient
import AlondraRelayOutbox
import AlondraSync
import CryptoKit
import Foundation

public enum RelayOperationResult: Sendable, Equatable {
    case completed(status: Int, body: Data)
    /// No replay is authorized, including after relaunch or a relay 404/410.
    case unknown
}

public enum RelayRecoveryStatus: Sendable, Equatable {
    case notDispatched
    case completed(status: Int, body: Data)
    case unknown
}

public enum RelaySessionError: Error { case invalidRequest, invalidResponse, operationIdentityConflict, outcomeUnknown, localStatus(Int) }

/// One immutable approved pairing. Replace the entire service when identity or keys change.
public actor RelaySession: SyncTransport, ServiceOperationTransport, ServicePushTransport {
    private let client: any ServiceTransport
    private let outbox: RelayOutbox
    private let scope: RelayOutboxScope
    private let codec: ServiceEnvelopeCodec
    private let sleep: @Sendable () async throws -> Void
    private var active = true

    public init(journalFile: URL, scope: RelayOutboxScope,
                bearer: @escaping @Sendable () async throws -> String,
                transport: (any ServiceHTTPTransport)? = nil,
                signing: P256.Signing.PrivateKey, agreement: P256.KeyAgreement.PrivateKey,
                macSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PublicKey,
                now: @escaping @Sendable () -> UInt64 = { UInt64(Date().timeIntervalSince1970 * 1000) },
                sleep: @escaping @Sendable () async throws -> Void = { try await Task.sleep(for: .seconds(1)) }) throws {
        let client = try AlondraAPIClient.makeServiceTransport(base: scope.relay, deviceID: scope.deviceID,
            signing: signing, bearer: bearer, transport: transport)
        self.client = client; self.scope = scope
        outbox = RelayOutbox(file: journalFile, scope: scope, transport: client)
        codec = ServiceEnvelopeCodec(macID: scope.macID, deviceID: scope.deviceID, signing: signing,
            agreement: agreement, macSigning: macSigning, macAgreement: macAgreement, now: now)
        self.sleep = sleep
    }

    public func registerServicePush(token: String, environment: String, binding: String) async throws {
        guard active else { throw CancellationError() }
        let registration = try RelayPushRegistration(token: token, environment: environment, pushBinding: binding)
        _ = try await client.bootstrap()
        guard active, !Task.isCancelled else { throw CancellationError() }
        try await client.registerPush(registration)
        guard active, !Task.isCancelled else { throw CancellationError() }
    }

    public func deactivate() { active = false }

    /// Product callers use AlondraAPIClient. This capability preserves the caller's durable
    /// operation identity through encrypted dispatch and status-only reconciliation.
    public func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        switch try await execute(requestID: request.operationID, method: request.method,
                                 path: request.path, body: request.body) {
        case let .completed(status, body): return .completed(status: status, body: body)
        case .unknown: return .unknown
        }
    }


    /// Caller persists the semantic operation ID before invoking this method. Reusing it only
    /// reconciles the existing envelope; it can never submit the mutation a second time.
    public func execute(requestID: String, method: String, path: String, body: Data? = nil,
                        statusPolls: Int = 10) async throws -> RelayOperationResult {
        guard active else { throw CancellationError() }
        guard ["GET", "POST", "PUT", "PATCH", "DELETE"].contains(method),
              path.hasPrefix("/v1/"), !path.contains("#"), statusPolls >= 0, statusPolls <= 30 else {
            throw RelaySessionError.invalidRequest
        }
        if method == "GET" {
            // Read identities are private and never share the durable mutation namespace.
            return try await read(requestID: "read-" + UUID().uuidString, path: path, body: body, statusPolls: statusPolls)
        }
        guard !requestID.hasPrefix("read-") else { throw RelaySessionError.invalidRequest }
        let tag = try codec.semanticTag(method: method, path: path, body: body)
        if let existing = try await outbox.entry(requestID: requestID) {
            guard existing.semanticTag == tag else { throw RelaySessionError.operationIdentityConflict }
            return try await resume(requestID: requestID, statusPolls: statusPolls)
        }
        // Bootstrap is read-only. A restart between this read and POST may still cause
        // uncertainty; only status reconciliation may follow that POST, never a replay.
        _ = try await client.bootstrap()
        guard active else { throw CancellationError() }
        let envelope = try sealRequest(requestID: requestID, method: method, path: path, body: body)
        try Task.checkCancellation()
        let submitted: RelaySubmission
        do { submitted = try await outbox.submitNew(envelope, semanticTag: tag) }
        catch RelayOutboxError.duplicateRequestID {
            guard let existing = try await outbox.entry(requestID: requestID), existing.semanticTag == tag
            else { throw RelaySessionError.operationIdentityConflict }
            return try await resume(requestID: requestID, statusPolls: statusPolls)
        }
        if case let .accepted(receipt) = submitted, let result = try verified(receipt, requestID: requestID) { return result }
        return try await resume(requestID: requestID, statusPolls: statusPolls)
    }

    private func sealRequest(requestID: String, method: String, path: String, body: Data?) throws -> EnvelopeV1 {
        do { return try codec.sealRequest(requestID: requestID, method: method, path: path, body: body) }
        catch ServiceEnvelopeError.invalidRequest { throw RelaySessionError.invalidRequest }
    }

    /// Reads have no durable side effect to deduplicate. Keep their request lifecycle in this
    /// call only, so periodic synchronization cannot accumulate permanent journal records.
    private func read(requestID: String, path: String, body: Data?, statusPolls: Int) async throws -> RelayOperationResult {
        _ = try await client.bootstrap()
        guard active else { throw CancellationError() }
        let envelope = try sealRequest(requestID: requestID, method: "GET", path: path, body: body)
        try Task.checkCancellation()
        do {
            let receipt = try await client.submitRequest(envelope)
            if let result = try verified(receipt, requestID: requestID) { return result }
        } catch is CancellationError { throw CancellationError() }
        catch {
            // A lost read response is reconciled without another POST in this invocation.
        }
        for attempt in 0..<statusPolls {
            try Task.checkCancellation()
            if attempt > 0 { try await sleep() }
            guard active else { throw CancellationError() }
            do {
                let receipt: RelayReceipt
                do { receipt = try await client.requestStatus(requestID: requestID) }
                catch RelayWireError.unauthorized {
                    _ = try await client.bootstrap()
                    guard active else { throw CancellationError() }
                    receipt = try await client.requestStatus(requestID: requestID)
                }
                if let result = try verified(receipt, requestID: requestID) { return result }
            } catch is CancellationError { throw CancellationError() }
            catch { return .unknown }
        }
        return .unknown
    }

    /// Absence is authoritative only because outbox entries/tombstones are never deleted.
    /// A journal read error propagates; it must never be interpreted as permission to send.
    public func recoveryStatus(requestID: String) async throws -> RelayRecoveryStatus {
        guard active else { return .unknown }
        guard try await outbox.entry(requestID: requestID) != nil else { return .notDispatched }
        switch try await resume(requestID: requestID) {
        case .completed(let status, let body): return .completed(status: status, body: body)
        case .unknown: return .unknown
        }
    }

    public func resume(requestID: String, statusPolls: Int = 10) async throws -> RelayOperationResult {
        guard active else { return .unknown }
        guard (0...30).contains(statusPolls) else { throw RelaySessionError.invalidRequest }
        guard let entry = try await outbox.entry(requestID: requestID) else { return .unknown }
        if let status = entry.lastStatus, let revision = entry.lastRevision,
           let result = try verified(RelayReceipt(requestID: requestID, status: status,
                revision: revision, response: entry.lastResponse), requestID: requestID) { return result }
        for attempt in 0..<statusPolls {
            do {
                try Task.checkCancellation()
                if attempt > 0 { try await sleep() }
                guard active else { return .unknown }
                if let receipt = try await reconcileRead(requestID: requestID),
                   let result = try verified(receipt, requestID: requestID) { return result }
            } catch is CancellationError { return .unknown }
            catch { return .unknown }
        }
        return .unknown
    }

    private func reconcileRead(requestID: String) async throws -> RelayReceipt? {
        do { return try await outbox.reconcile(requestID: requestID) }
        catch RelayWireError.unauthorized {
            // Authentication checks the challenge epoch before the query cursor. Refresh
            // it once on a read; revoked credentials still fail on the second GET.
            _ = try await client.bootstrap()
            guard active else { throw CancellationError() }
            return try await outbox.reconcile(requestID: requestID)
        }
    }

    private func verified(_ receipt: RelayReceipt, requestID: String) throws -> RelayOperationResult? {
        guard active else { return .unknown }
        guard let result = try codec.verified(receipt, requestID: requestID) else { return nil }
        switch result {
        case .unknown: return .unknown
        case let .completed(status, body): return .completed(status: status, body: body)
        }
    }

    /// Only after the use case durably records a verified result. Retains the outbox tombstone.
    public func retireAppliedResponse(requestID: String) async throws {
        guard let entry = try await outbox.entry(requestID: requestID),
              let revision = entry.lastRevision, let response = entry.lastResponse,
              let status = entry.lastStatus,
              case .completed = try verified(RelayReceipt(requestID: requestID, status: status,
                  revision: revision, response: response), requestID: requestID)
        else { throw RelaySessionError.outcomeUnknown }
        try await outbox.retireAppliedResponse(requestID: requestID, revision: revision, verifiedResponse: response)
    }

    public func fetchOverview() async throws -> Data {
        let requestID = UUID().uuidString
        let result = try await execute(requestID: requestID, method: "GET", path: "/v1/overview")
        guard case let .completed(status, body) = result else { throw RelaySessionError.outcomeUnknown }
        guard status == 200 else { throw RelaySessionError.localStatus(status) }
        _ = try SyncSnapshot(body: body)
        return body
    }

    /// Relay changes wake this read's status query; they never invalidate daemon content
    /// themselves. The completed response still crosses the same authenticated envelope gate.
    private func waitForReadCompletion(requestID: String, path: String, seconds: Int) async throws -> RelayOperationResult {
        let clock = ContinuousClock()
        let bootstrap = try await client.bootstrap()
        try Task.checkCancellation()
        guard active else { throw CancellationError() }
        let envelope = try sealRequest(requestID: requestID, method: "GET", path: path, body: nil)
        try Task.checkCancellation()
        do {
            let receipt: RelayReceipt
            do {
                receipt = try await client.submitRequest(envelope)
            } catch is CancellationError { throw CancellationError() }
            catch {
                // A lost submission response authorizes only GET reconciliation, never POST.
                try Task.checkCancellation()
                guard active else { throw CancellationError() }
                receipt = try await client.requestStatus(requestID: requestID)
            }
            try Task.checkCancellation()
            guard active else { throw CancellationError() }
            if let result = try verified(receipt, requestID: requestID) { return result }
            if receipt.status == "expired" { return .unknown }
            let deadline = clock.now.advanced(by: .seconds(seconds + 5))
            var revision = receipt.revision
            while clock.now < deadline {
                try Task.checkCancellation()
                guard active else { throw CancellationError() }
                let remaining = clock.now.duration(to: deadline).components
                let wait = UInt64(max(1, min(25, remaining.seconds + (remaining.attoseconds > 0 ? 1 : 0))))
                let update = try await client.changes(since: revision, epoch: bootstrap.epoch, waitSeconds: wait)
                try Task.checkCancellation()
                guard active else { throw CancellationError() }
                // A new relay epoch or a backwards cursor cannot describe this job's lifecycle.
                guard update.epoch == bootstrap.epoch, update.revision >= revision else { return .unknown }
                revision = update.revision
                guard let job = update.changes.first(where: { $0.requestID == requestID }) else { continue }
                if job.status == "expired" { return .unknown }
                guard job.status == "completed" else { continue }
                let completed = try await client.requestStatus(requestID: requestID)
                try Task.checkCancellation()
                guard active else { throw CancellationError() }
                if let result = try verified(completed, requestID: requestID) { return result }
            }
            return .unknown
        } catch is CancellationError { throw CancellationError() }
        catch { return .unknown }
    }

    public func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        guard active else { throw CancellationError() }
        let duration = max(0, min(seconds, 50))
        // Wait on daemon content changes inside the encrypted service request. Relay job
        // lifecycle events include our own reads and cannot be used as content invalidation.
        let result = try await waitForReadCompletion(requestID: "read-" + UUID().uuidString,
            path: "/v1/changes?after=\(afterSnapshotTick)&wait=\(duration)", seconds: duration)
        try Task.checkCancellation()
        guard active else { throw CancellationError() }
        switch result {
        case .completed(let status, _) where (200..<300).contains(status): return
        case .completed(let status, _): throw RelaySessionError.localStatus(status)
        case .unknown: throw RelaySessionError.outcomeUnknown
        }
    }
}
