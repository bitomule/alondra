import AlondraAPIClient
import AlondraRelayClient
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
public actor RelaySession: SyncTransport, ServiceOperationTransport {
    private let client: AlondraRelayClient
    private let outbox: RelayOutbox
    private let scope: RelayOutboxScope
    private let signing: P256.Signing.PrivateKey
    private let agreement: P256.KeyAgreement.PrivateKey
    private let macSigning: P256.Signing.PublicKey
    private let macAgreement: P256.KeyAgreement.PublicKey
    private let now: @Sendable () -> UInt64
    private let sleep: @Sendable () async throws -> Void
    private var active = true
    private let wait: @Sendable (Duration) async throws -> Void

    public init(journalFile: URL, scope: RelayOutboxScope,
                bearer: @escaping @Sendable () async throws -> String,
                transport: any RelayHTTPTransport = URLSessionRelayTransport(),
                signing: P256.Signing.PrivateKey, agreement: P256.KeyAgreement.PrivateKey,
                macSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PublicKey,
                now: @escaping @Sendable () -> UInt64 = { UInt64(Date().timeIntervalSince1970 * 1000) },
                wait: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
                sleep: @escaping @Sendable () async throws -> Void = { try await Task.sleep(for: .seconds(1)) }) throws {
        let client = try AlondraRelayClient(base: scope.relay, deviceID: scope.deviceID,
            signer: { try signing.signature(for: $0).rawRepresentation }, bearer: bearer, transport: transport)
        self.client = client; self.scope = scope
        outbox = RelayOutbox(file: journalFile, scope: scope, transport: client)
        self.signing = signing; self.agreement = agreement
        self.macSigning = macSigning; self.macAgreement = macAgreement
        self.now = now; self.sleep = sleep; self.wait = wait
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
        let tag = try semanticTag(method: method, path: path, body: body)
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

    private func requestBytes(method: String, path: String, body: Data?) throws -> Data {
        var payload: [String: Any] = ["method": method, "path": path]
        if let body { payload["body"] = try JSONSerialization.jsonObject(with: body, options: [.fragmentsAllowed]) }
        return try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    }

    private func semanticTag(method: String, path: String, body: Data?) throws -> Data {
        let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: agreement.rawRepresentation),
            salt: Data(), info: Data("alondra-operation-binding-v1".utf8), outputByteCount: 32)
        return Data(HMAC<SHA256>.authenticationCode(for: try requestBytes(method: method, path: path, body: body), using: key))
    }

    private func sealRequest(requestID: String, method: String, path: String, body: Data?) throws -> EnvelopeV1 {
        let plaintext = try requestBytes(method: method, path: path, body: body)
        let timestamp = now()
        guard timestamp <= UInt64.max - 120_000 else { throw RelaySessionError.invalidRequest }
        return try EnvelopeV1.seal(route: scope.macID, sender: scope.deviceID,
            recipient: scope.macID, requestID: requestID, kind: "request",
            expiresAtMilliseconds: timestamp + 120_000, plaintext: plaintext,
            senderKey: signing, recipientKey: macAgreement)
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
        guard receipt.requestID == requestID else { throw RelaySessionError.invalidResponse }
        guard receipt.status == "completed" else { return nil }
        guard let envelope = receipt.response, envelope.kind == "response", envelope.requestID == requestID else {
            throw RelaySessionError.invalidResponse
        }
        let bytes = try envelope.open(senderKey: macSigning, recipientKey: agreement,
            sender: scope.macID, recipient: scope.deviceID, route: scope.macID, nowMilliseconds: now())
        guard let json = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              let status = json["status"] as? Int, (100...599).contains(status) else { throw RelaySessionError.invalidResponse }
        if json["error"] as? String == "uncertain" || status >= 500 { return .unknown }
        let body = try JSONSerialization.data(withJSONObject: json["body"] ?? json, options: [.fragmentsAllowed, .sortedKeys])
        return .completed(status: status, body: body)
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

    public func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        guard active else { throw CancellationError() }
        // /v1/changes reports relay job lifecycle, including our own overview request.
        // It is not a daemon-data invalidation feed. Poll at a bounded cadence instead
        // of feeding our own completion back into another immediate overview.
        try await wait(.seconds(max(1, min(seconds, 25))))
        try Task.checkCancellation()
        guard active else { throw CancellationError() }
    }
}
