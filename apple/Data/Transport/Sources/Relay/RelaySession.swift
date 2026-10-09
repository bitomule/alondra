import AlondraContract
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

public enum RelaySessionError: Error, Equatable { case invalidRequest, invalidResponse, operationIdentityConflict, outcomeUnknown }

/// One immutable approved pairing. Replace the entire service when identity or keys change.
public actor RelaySession: OperationAPI, PushRegistrationAPI {
    private let client: any Transport
    private let outbox: RelayOutbox
    private let scope: RelayOutboxScope
    private let codec: EnvelopeCodec
    private let sleep: @Sendable () async throws -> Void
    private let requestClock: RequestClock
    let mutations: MutationRunner
    let mutationEvidence = MutationEvidenceStore()
    private let waitForReadDeadline: @Sendable (ContinuousClock.Instant) async throws -> Void
    private var active = true
    private var relayBootstrap: RelayBootstrap?

    public init(scope: RelayOutboxScope,
                bearer: @escaping @Sendable () async throws -> String,
                bearerRefused: @escaping @Sendable () async -> Void = {},
                transport: (any HTTPTransport)? = nil,
                signing: P256.Signing.PrivateKey, agreement: P256.KeyAgreement.PrivateKey,
                macSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PublicKey,
                now: @escaping @Sendable () -> UInt64 = { UInt64(Date().timeIntervalSince1970 * 1000) },
                sleep: @escaping @Sendable () async throws -> Void = { try await Task.sleep(for: .seconds(1)) }) throws {
        try self.init(scope: scope, bearer: bearer, bearerRefused: bearerRefused, transport: transport,
            signing: signing, agreement: agreement, macSigning: macSigning, macAgreement: macAgreement,
            now: now, sleep: sleep, waitForReadDeadline: { try await ContinuousClock().sleep(until: $0) })
    }

    init(scope: RelayOutboxScope,
                bearer: @escaping @Sendable () async throws -> String,
                bearerRefused: @escaping @Sendable () async -> Void = {},
                transport: (any HTTPTransport)? = nil,
                signing: P256.Signing.PrivateKey, agreement: P256.KeyAgreement.PrivateKey,
                macSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PublicKey,
                now: @escaping @Sendable () -> UInt64 = { UInt64(Date().timeIntervalSince1970 * 1000) },
                sleep: @escaping @Sendable () async throws -> Void = { try await Task.sleep(for: .seconds(1)) },
                waitForReadDeadline: @escaping @Sendable (ContinuousClock.Instant) async throws -> Void,
                requestClock: RequestClock = .continuous) throws {
        let client = try AlondraAPIClient.makeTransport(base: scope.relay, deviceID: scope.deviceID,
            signing: signing, bearer: bearer, bearerRefused: bearerRefused, transport: transport)
        self.client = client; self.scope = scope
        outbox = RelayOutbox(scope: scope, transport: client)
        codec = EnvelopeCodec(macID: scope.macID, deviceID: scope.deviceID, signing: signing,
            agreement: agreement, macSigning: macSigning, macAgreement: macAgreement, now: now)
        self.sleep = sleep
        self.requestClock = requestClock
        mutations = MutationRunner(clock: requestClock)
        self.waitForReadDeadline = waitForReadDeadline
    }

    public func registerServicePush(token: String, environment: String, binding: String) async throws {
        guard active else { throw CancellationError() }
        let registration = try RelayPushRegistration(token: token, environment: environment, pushBinding: binding)
        _ = try await client.bootstrap()
        guard active, !Task.isCancelled else { throw CancellationError() }
        try await client.registerPush(registration)
        guard active, !Task.isCancelled else { throw CancellationError() }
    }

    public func deactivate() async {
        active = false
        await mutations.cancelAll()
    }

    /// The envelope this session sent for an operation, so another carrier can resend the very
    /// same bytes: the Mac's journal recognises them and never runs the operation twice.
    public func sentEnvelope(for requestID: String) async -> Envelope? {
        try? await outbox.entry(requestID: requestID)?.envelope
    }

    /// Product callers use AlondraAPIClient. This capability preserves the caller's durable
    /// operation identity through encrypted dispatch and status-only reconciliation.
    public func perform(_ request: APIRequest) async throws -> APIResponse {
        if request.method == "GET", request.path.hasPrefix("/changes?"), request.body == nil,
           let wait = EnvelopeRequestPolicy.viewsChangeWait(request.path) {
            switch try await waitForReadCompletion(requestID: "read-" + UUID().uuidString, path: request.path,
                                                   seconds: wait, deadlineSlack: 20) {
            case let .completed(status, body): return .completed(status: status, body: body)
            case .unknown: return .unknown
            }
        }
        if request.method == "GET", request.path.hasPrefix("/changes?"), request.body == nil {
            guard let components = URLComponents(string: request.path),
                  let items = components.queryItems,
                  Set(items.map(\.name)) == ["data_epoch", "after", "wait"], items.count == 3,
                  let wait = items.first(where: { $0.name == "wait" })?.value.flatMap(Int.init), (0...50).contains(wait),
                  items.first(where: { $0.name == "data_epoch" })?.value?.isEmpty == false,
                  items.first(where: { $0.name == "after" })?.value.flatMap(UInt64.init) != nil
            else { throw RelaySessionError.invalidRequest }
            switch try await waitForReadCompletion(requestID: "read-" + UUID().uuidString, path: request.path, seconds: wait) {
            case let .completed(status, body): return .completed(status: status, body: body)
            case .unknown: return .unknown
            }
        }
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
        guard method == "POST" else {
            return try await executePrepared(requestID: requestID, method: method, path: path, body: body, statusPolls: statusPolls)
        }
        let startedAt = requestClock.now()
        let owner = MutationScope.current?.id ?? UUID()
        let lease = mutationEvidence.acquire(requestID, owner: owner)
        defer { mutationEvidence.release(lease) }
        return try await mutations.run(dispatchEvidence: lease.evidence, ownerID: owner, startedAt: startedAt, unknown: { RelayOperationResult.unknown }) {
            try await self.executePrepared(requestID: requestID, method: method, path: path, body: body, statusPolls: statusPolls)
        }
    }

    private func executePrepared(requestID: String, method: String, path: String, body: Data?,
                                 statusPolls: Int) async throws -> RelayOperationResult {
        try MutationScope.check()
        guard active else { throw CancellationError() }
        guard ["GET", "POST"].contains(method),
              EnvelopeRequestPolicy.validViewsPath(method: method, path: path, hasBody: body != nil),
              !path.contains("#"), statusPolls >= 0, statusPolls <= 30 else {
            throw RelaySessionError.invalidRequest
        }
        if method == "GET" {
            // Read identities are private and never share the durable mutation namespace.
            guard body == nil else { throw RelaySessionError.invalidRequest }
            return try await waitForReadCompletion(requestID: "read-" + UUID().uuidString, path: path, seconds: statusPolls)
        }
        guard !requestID.hasPrefix("read-") else { throw RelaySessionError.invalidRequest }
        let tag = try codec.semanticTag(method: method, path: path, body: body)
        if let existing = try await outbox.entry(requestID: requestID) {
            try MutationScope.check()
            guard existing.semanticTag == tag else { throw RelaySessionError.operationIdentityConflict }
            return try await resume(requestID: requestID, statusPolls: statusPolls)
        }
        // Bootstrap is read-only. A restart between this read and POST may still cause
        // uncertainty; only status reconciliation may follow that POST, never a replay.
        _ = try await client.bootstrap()
        try MutationScope.check()
        guard active else { throw CancellationError() }
        let envelope = try sealRequest(requestID: requestID, method: method, path: path, body: body)
        try Task.checkCancellation()
        try MutationScope.check()
        let submitted: RelaySubmission
        let evidence = mutationEvidence
        do { submitted = try await outbox.submitNew(envelope, semanticTag: tag, reserved: { evidence.retain(requestID) }) }
        catch RelayOutboxError.duplicateRequestID {
            guard let existing = try await outbox.entry(requestID: requestID), existing.semanticTag == tag
            else { throw RelaySessionError.operationIdentityConflict }
            return try await resume(requestID: requestID, statusPolls: statusPolls)
        }
        try MutationScope.check()
        if case let .accepted(receipt) = submitted, let result = try verified(receipt, requestID: requestID) { return result }
        return try await resume(requestID: requestID, statusPolls: statusPolls)
    }

    private func sealRequest(requestID: String, method: String, path: String, body: Data?) throws -> Envelope {
        do { return try codec.sealRequest(requestID: requestID, method: method, path: path, body: body) }
        catch EnvelopeError.invalidRequest { throw RelaySessionError.invalidRequest }
    }

    /// Absence is authoritative only because outbox entries/tombstones are never deleted.
    /// A journal read error propagates; it must never be interpreted as permission to send.
    public func recoveryStatus(requestID: String) async throws -> RelayRecoveryStatus {
        return try await mutations.run(conservativeUnknown: true, startedAt: requestClock.now(), unknown: { RelayRecoveryStatus.unknown }) {
            try await self.recoveryPrepared(requestID: requestID)
        }
    }

    private func recoveryPrepared(requestID: String) async throws -> RelayRecoveryStatus {
        try MutationScope.check()
        guard active else { return .unknown }
        let entry = try await outbox.entry(requestID: requestID)
        try MutationScope.check()
        guard entry != nil else { return .notDispatched }
        switch try await resume(requestID: requestID) {
        case .completed(let status, let body): return .completed(status: status, body: body)
        case .unknown: return .unknown
        }
    }

    public func resume(requestID: String, statusPolls: Int = 10) async throws -> RelayOperationResult {
        return try await mutations.run(conservativeUnknown: true, startedAt: requestClock.now(), unknown: { RelayOperationResult.unknown }) {
            try await self.resumePrepared(requestID: requestID, statusPolls: statusPolls)
        }
    }

    private func resumePrepared(requestID: String, statusPolls: Int) async throws -> RelayOperationResult {
        try MutationScope.check()
        guard active else { return .unknown }
        guard (0...30).contains(statusPolls) else { throw RelaySessionError.invalidRequest }
        guard let entry = try await outbox.entry(requestID: requestID) else { return .unknown }
        try MutationScope.check()
        if let status = entry.lastStatus, let revision = entry.lastRevision,
           let result = try verified(RelayReceipt(requestID: requestID, status: status,
                revision: revision, response: entry.lastResponse), requestID: requestID) { return result }
        for attempt in 0..<statusPolls {
            do {
                try Task.checkCancellation()
                try MutationScope.check()
                if attempt > 0 { try await sleep() }
                try MutationScope.check()
                guard active else { return .unknown }
                if let receipt = try await reconcileRead(requestID: requestID),
                   let result = try verified(receipt, requestID: requestID) {
                    try MutationScope.check()
                    return result
                }
            } catch is CancellationError { return .unknown }
            catch { return .unknown }
        }
        return .unknown
    }

    private func reconcileRead(requestID: String) async throws -> RelayReceipt? {
        do { return try await outbox.reconcile(requestID: requestID) }
        catch RelayWireError.unauthorized {
            try MutationScope.check()
            // Authentication checks the challenge epoch before the query cursor. Refresh
            // it once on a read; revoked credentials still fail on the second GET.
            _ = try await client.bootstrap()
            guard active else { throw CancellationError() }
            try MutationScope.check()
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

    /// Relay changes wake this read's status query; they never invalidate daemon content
    /// themselves. The completed response still crosses the same authenticated envelope gate.
    private func waitForReadCompletion(requestID: String, path: String, seconds: Int,
                                       deadlineSlack: Int = 5) async throws -> RelayOperationResult {
        // Includes bootstrap, the one submission, status reconciliation and every HTTP wait.
        let deadline = ContinuousClock().now.advanced(by: .seconds(seconds + deadlineSlack))
        let waitForDeadline = waitForReadDeadline
        return try await withThrowingTaskGroup(of: RelayOperationResult.self) { group in
            group.addTask {
                try await waitForDeadline(deadline)
                try Task.checkCancellation()
                return .unknown
            }
            group.addTask { try await self.readUntilChange(requestID: requestID, path: path, deadline: deadline) }
            defer { group.cancelAll() }
            let result = try await group.next() ?? .unknown
            try Task.checkCancellation()
            guard ContinuousClock().now < deadline else { return .unknown }
            return result
        }
    }

    private func checkReadDeadline(_ deadline: ContinuousClock.Instant) throws {
        try Task.checkCancellation()
        guard active else { throw CancellationError() }
        guard ContinuousClock().now < deadline else { throw RelaySessionError.outcomeUnknown }
    }

    private func readUntilChange(requestID: String, path: String, deadline: ContinuousClock.Instant) async throws -> RelayOperationResult {
        let clock = ContinuousClock()
        var bootstrap = try await currentBootstrap()
        try checkReadDeadline(deadline)
        let envelope = try sealRequest(requestID: requestID, method: "GET", path: path, body: nil)
        try checkReadDeadline(deadline)
        do {
            let receipt: RelayReceipt
            do {
                do {
                    receipt = try await client.submitRequest(envelope)
                } catch RelayWireError.unauthorized {
                    // The relay refused the request outright, so nothing was queued: refresh the epoch once.
                    relayBootstrap = nil
                    try checkReadDeadline(deadline)
                    bootstrap = try await currentBootstrap()
                    try checkReadDeadline(deadline)
                    receipt = try await client.submitRequest(envelope)
                }
            } catch is CancellationError { throw CancellationError() }
            catch {
                // A lost submission response authorizes only GET reconciliation, never POST.
                try checkReadDeadline(deadline)
                receipt = try await client.requestStatus(requestID: requestID)
            }
            try checkReadDeadline(deadline)
            if let result = try verified(receipt, requestID: requestID) { return result }
            if receipt.status == "expired" { return .unknown }
            var revision = receipt.revision
            while clock.now < deadline {
                try checkReadDeadline(deadline)
                let remaining = clock.now.duration(to: deadline).components
                let wait = UInt64(max(1, min(25, remaining.seconds + (remaining.attoseconds > 0 ? 1 : 0))))
                let update = try await client.changes(since: revision, epoch: bootstrap.epoch, waitSeconds: wait)
                try checkReadDeadline(deadline)
                // A new relay epoch or a backwards cursor cannot describe this job's lifecycle.
                guard update.epoch == bootstrap.epoch, update.revision >= revision else { return .unknown }
                revision = update.revision
                guard let job = update.changes.first(where: { $0.requestID == requestID }) else { continue }
                if job.status == "expired" { return .unknown }
                guard job.status == "completed" else { continue }
                let completed = try await client.requestStatus(requestID: requestID)
                try checkReadDeadline(deadline)
                if let result = try verified(completed, requestID: requestID) { return result }
            }
            return .unknown
        } catch is CancellationError { throw CancellationError() }
        catch {
            if case RelayWireError.unauthorized = error { relayBootstrap = nil }
            if case RelayWireError.conflict = error { relayBootstrap = nil }
            return .unknown
        }
    }

    /// The relay epoch only changes when the relay restarts; a rejection clears it.
    private func currentBootstrap() async throws -> RelayBootstrap {
        if let relayBootstrap { return relayBootstrap }
        let fresh = try await client.bootstrap()
        relayBootstrap = fresh
        return fresh
    }
}
