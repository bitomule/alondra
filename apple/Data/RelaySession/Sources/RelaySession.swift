import AlondraContracts
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
public actor RelaySession: ServiceOperationTransport, ServicePushTransport {
    private let client: any ServiceTransport
    private let makeSnapshotClient: @Sendable () async throws -> (any ServiceTransport, @Sendable () async throws -> String)
    private let outbox: RelayOutbox
    private let scope: RelayOutboxScope
    private let codec: ServiceEnvelopeCodec
    private let sleep: @Sendable () async throws -> Void
    private let waitForReadDeadline: @Sendable (ContinuousClock.Instant) async throws -> Void
    private var active = true

    public init(scope: RelayOutboxScope,
                bearer: @escaping @Sendable () async throws -> String,
                cachedBearer: @escaping @Sendable () async throws -> @Sendable () async throws -> String = { throw RelaySessionError.outcomeUnknown },
                transport: (any ServiceHTTPTransport)? = nil,
                signing: P256.Signing.PrivateKey, agreement: P256.KeyAgreement.PrivateKey,
                macSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PublicKey,
                now: @escaping @Sendable () -> UInt64 = { UInt64(Date().timeIntervalSince1970 * 1000) },
                sleep: @escaping @Sendable () async throws -> Void = { try await Task.sleep(for: .seconds(1)) }) throws {
        try self.init(scope: scope, bearer: bearer, cachedBearer: cachedBearer, transport: transport,
            signing: signing, agreement: agreement, macSigning: macSigning, macAgreement: macAgreement,
            now: now, sleep: sleep, waitForReadDeadline: { try await ContinuousClock().sleep(until: $0) })
    }

    init(scope: RelayOutboxScope,
                bearer: @escaping @Sendable () async throws -> String,
                cachedBearer: @escaping @Sendable () async throws -> @Sendable () async throws -> String = { throw RelaySessionError.outcomeUnknown },
                transport: (any ServiceHTTPTransport)? = nil,
                signing: P256.Signing.PrivateKey, agreement: P256.KeyAgreement.PrivateKey,
                macSigning: P256.Signing.PublicKey, macAgreement: P256.KeyAgreement.PublicKey,
                now: @escaping @Sendable () -> UInt64 = { UInt64(Date().timeIntervalSince1970 * 1000) },
                sleep: @escaping @Sendable () async throws -> Void = { try await Task.sleep(for: .seconds(1)) },
                waitForReadDeadline: @escaping @Sendable (ContinuousClock.Instant) async throws -> Void) throws {
        let client = try AlondraAPIClient.makeServiceTransport(base: scope.relay, deviceID: scope.deviceID,
            signing: signing, bearer: bearer, transport: transport)
        self.client = client; self.scope = scope
        makeSnapshotClient = {
            let lease = try await cachedBearer()
            return (try AlondraAPIClient.makeServiceTransport(base: scope.relay, deviceID: scope.deviceID,
                signing: signing, bearer: lease, transport: transport), lease)
        }
        outbox = RelayOutbox(scope: scope, transport: client)
        codec = ServiceEnvelopeCodec(macID: scope.macID, deviceID: scope.deviceID, signing: signing,
            agreement: agreement, macSigning: macSigning, macAgreement: macAgreement, now: now)
        self.sleep = sleep
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

    public func deactivate() { active = false }

    /// Product callers use AlondraAPIClient. This capability preserves the caller's durable
    /// operation identity through encrypted dispatch and status-only reconciliation.
    public func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        if request.method == "GET", request.path == "/v2/domain", request.body == nil {
            return .completed(status: 200, body: try await fetchDomainProjection())
        }
        if request.method == "GET", request.path.hasPrefix("/v2/domain/changes?"), request.body == nil {
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
        guard active else { throw CancellationError() }
        guard ["GET", "POST", "PUT", "PATCH", "DELETE"].contains(method),
              ((method == "GET" && path == "/v2/domain/capabilities")
               || (method == "POST" && path == "/v2/domain/commands")
               || Self.validDomainDeliveryPath(method: method, path: path)
               || Self.validDomainSchedulePath(method: method, path: path, hasBody: body != nil)
               || Self.validClientContractPath(method: method, path: path)
               || (method == "GET" && (path.hasPrefix("/v2/domain/pull-requests/") && path.hasSuffix("/diff")
                   || path.hasPrefix("/v2/domain/document-revisions/") && path.contains("/content?offset=")
                   || path.hasPrefix("/v2/domain/tasks/") && path.hasSuffix("/actions")))),
              !path.contains("#"), statusPolls >= 0, statusPolls <= 30 else {
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

    /// Delivery recovery is a separate v2 resource. Keep the direct transport
    /// allowlist as strict as ServiceRequest so internal callers cannot send a
    /// similarly prefixed path with an arbitrary method or target.
    static func validDomainDeliveryPath(method: String, path: String) -> Bool {
        if method == "GET", path.hasPrefix("/v2/domain/delivery-attempts?after=") {
            let suffix = path.dropFirst("/v2/domain/delivery-attempts?after=".count)
            return !suffix.isEmpty && suffix.allSatisfy(\.isNumber) && Int64(suffix) != nil
        }
        guard method == "POST" else { return false }
        let components = path.split(separator: "/")
        return components.count == 5 && components[0] == "v2" && components[1] == "domain"
            && components[2] == "delivery-attempts" && components[4] == "resolve"
            && Int64(components[3]).map { $0 > 0 } == true
    }

    static func validClientContractPath(method: String, path: String) -> Bool {
        guard let method = HTTPMethod(rawValue: method), path.hasPrefix("/") else { return false }
        let route = path.split(separator: "?", maxSplits: 1).first.map(String.init) ?? path
        let segments = route.split(separator: "/", omittingEmptySubsequences: false).dropFirst().map(String.init)
        return ServiceRequest.validClientContractRoute(method: method, path: segments)
    }

    /// Mirror the Mac's exact Schedule v2 operation shapes before sealing.
    static func validDomainSchedulePath(method: String, path: String, hasBody: Bool) -> Bool {
        if path == "/v2/domain/schedules" {
            return (method == "GET" && !hasBody) || (method == "POST" && hasBody)
        }
        let prefix = "/v2/domain/schedules/"
        guard path.hasPrefix(prefix) else { return false }
        let suffix = String(path.dropFirst(prefix.count))
        let isRun = suffix.hasSuffix("/run")
        let id = isRun ? String(suffix.dropLast(4)) : String(suffix)
        guard (1...256).contains(id.utf8.count), id.utf8.allSatisfy({ byte in
            (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122)
                || (byte >= 48 && byte <= 57) || byte == 45 || byte == 95
        }) else { return false }
        if isRun { return method == "POST" && !hasBody }
        return (method == "GET" && !hasBody) || (method == "PUT" && hasBody)
            || (method == "DELETE" && !hasBody)
    }

    private func sealRequest(requestID: String, method: String, path: String, body: Data?) throws -> EnvelopeV1 {
        do { return try codec.sealRequest(requestID: requestID, method: method, path: path, body: body) }
        catch ServiceEnvelopeError.invalidRequest { throw RelaySessionError.invalidRequest }
    }

    /// Reads have no durable side effect to deduplicate. Keep their request lifecycle in this
    /// call only, so periodic synchronization cannot accumulate permanent journal records.
    private func read(requestID: String, path: String, body: Data?, statusPolls: Int, allowOverviewTooLarge: Bool = false) async throws -> RelayOperationResult {
        _ = try await client.bootstrap()
        guard active else { throw CancellationError() }
        let envelope = try sealRequest(requestID: requestID, method: "GET", path: path, body: body)
        try Task.checkCancellation()
        do {
            let receipt = try await client.submitRequest(envelope)
            if let result = try verified(receipt, requestID: requestID, allowOverviewTooLarge: allowOverviewTooLarge) { return result }
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
                if let result = try verified(receipt, requestID: requestID, allowOverviewTooLarge: allowOverviewTooLarge) { return result }
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

    private func verified(_ receipt: RelayReceipt, requestID: String, allowOverviewTooLarge: Bool = false) throws -> RelayOperationResult? {
        guard active else { return .unknown }
        guard let result = try codec.verified(receipt, requestID: requestID, allowOverviewTooLarge: allowOverviewTooLarge) else { return nil }
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

    private func fetchDomainProjection() async throws -> Data {
        let result = try await read(requestID: "read-" + UUID().uuidString, path: "/v2/domain", body: nil,
                                    statusPolls: 10, allowOverviewTooLarge: true)
        guard case let .completed(status, body) = result else { throw RelaySessionError.outcomeUnknown }
        let validate: @Sendable (Data) throws -> Void = { bytes in
            let projection = try DomainV2.decode(DomainV2.DomainProjection.self, from: bytes)
            guard projection.protocolVersion == DomainV2.protocolVersion,
                  projection.revision <= DomainV2.maximumRevision else { throw RelaySessionError.invalidResponse }
        }
        if status == 502 { return try await fetchSnapshot(root: "/v2/domain/snapshot", validate: validate) }
        guard status == 200 else { throw RelaySessionError.localStatus(status) }
        try Task.checkCancellation()
        guard active else { throw CancellationError() }
        try validate(body)
        return body
    }

    /// Relay changes wake this read's status query; they never invalidate daemon content
    /// themselves. The completed response still crosses the same authenticated envelope gate.
    private func waitForReadCompletion(requestID: String, path: String, seconds: Int) async throws -> RelayOperationResult {
        // Includes bootstrap, the one submission, status reconciliation and every HTTP wait.
        let deadline = ContinuousClock().now.advanced(by: .seconds(seconds + 5))
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

    /// Each operation has a structured deadline and uses only a cached credential.
    /// The initial ordinary overview read already performed normal authentication.
    private func fetchSnapshot(root: String, validate: @Sendable (Data) throws -> Void) async throws -> Data {
        let deadline = ContinuousClock.now.advanced(by: .seconds(90))
        let (snapshotClient, lease) = try await makeSnapshotClient()
        try checkReadDeadline(deadline)
        var cleanupPath: String?
        do {
            let first = try await snapshotOperation(client: snapshotClient, method: "GET", path: root, deadline: deadline)
            // Retain a syntactically safe handle even if the remaining metadata is invalid.
            if let header = try? JSONDecoder().decode(OverviewSnapshotAssembly.Chunk.self, from: first),
               OverviewSnapshotAssembly.hex(header.snapshot_id, count: 32) {
                cleanupPath = root + "/" + header.snapshot_id
            }
            var assembly = try OverviewSnapshotAssembly(first, root: root)
            while !assembly.complete {
                let chunk = try await snapshotOperation(client: snapshotClient, method: "GET", path: assembly.path + "?offset=\(assembly.bytes.count)", deadline: deadline)
                try assembly.append(chunk)
            }
            let result = try assembly.finish()
            try validate(result)
            try checkReadDeadline(deadline)
            let releasePath = cleanupPath
            cleanupPath = nil
            if let releasePath { await cleanupSnapshot(releasePath, client: snapshotClient, deadline: deadline) }
            // Cleanup failure/timeout cannot invalidate a document completed in budget.
            _ = try await lease()
            try Task.checkCancellation()
            guard active else { throw CancellationError() }
            return result
        } catch {
            if let cleanupPath { await cleanupSnapshot(cleanupPath, client: snapshotClient, deadline: deadline) }
            throw error
        }
    }

    private func cleanupSnapshot(_ path: String, client snapshotClient: any ServiceTransport, deadline: ContinuousClock.Instant) async {
        // Cancellation/logout may prevent cleanup; the Mac's TTL is the backstop.
        guard active, !Task.isCancelled, ContinuousClock.now < deadline else { return }
        let cleanupDeadline = min(deadline, ContinuousClock.now.advanced(by: .seconds(2)))
        _ = try? await snapshotOperation(client: snapshotClient, method: "DELETE", path: path, deadline: cleanupDeadline)
    }

    private func snapshotOperation(client snapshotClient: any ServiceTransport, method: String, path: String, deadline: ContinuousClock.Instant) async throws -> Data {
        let value = try await withThrowingTaskGroup(of: Data.self) { group in
            group.addTask { [waitForReadDeadline] in
                try await waitForReadDeadline(deadline)
                throw RelaySessionError.outcomeUnknown
            }
            group.addTask { try await self.snapshotExchange(client: snapshotClient, method: method, path: path, deadline: deadline) }
            defer { group.cancelAll() }
            guard let value = try await group.next() else { throw RelaySessionError.outcomeUnknown }
            return value
        }
        try checkReadDeadline(deadline)
        return value
    }

    private func snapshotExchange(client snapshotClient: any ServiceTransport, method: String, path: String, deadline: ContinuousClock.Instant) async throws -> Data {
        try checkReadDeadline(deadline)
        let bootstrap = try await snapshotClient.bootstrap()
        try checkReadDeadline(deadline)
        let requestID = "read-" + UUID().uuidString
        let envelope = try sealRequest(requestID: requestID, method: method, path: path, body: nil)
        var receipt: RelayReceipt
        do { receipt = try await snapshotClient.submitRequest(envelope) }
        catch is CancellationError { throw CancellationError() }
        catch {
            // Reconcile a lost POST, never resubmit it, including cleanup DELETE.
            try checkReadDeadline(deadline)
            receipt = try await snapshotClient.requestStatus(requestID: requestID)
        }
        var revision = receipt.revision
        while true {
            try checkReadDeadline(deadline)
            if let response = try verified(receipt, requestID: requestID) {
                guard case let .completed(status, body) = response else { throw RelaySessionError.outcomeUnknown }
                guard status == 200 else { throw RelaySessionError.localStatus(status) }
                return body
            }
            let remaining = ContinuousClock.now.duration(to: deadline).components.seconds
            guard remaining > 0 else { throw RelaySessionError.outcomeUnknown }
            let changes = try await snapshotClient.changes(since: revision, epoch: bootstrap.epoch,
                waitSeconds: UInt64(min(25, remaining)))
            try checkReadDeadline(deadline)
            guard changes.epoch == bootstrap.epoch, changes.revision >= revision else {
                throw RelaySessionError.invalidResponse
            }
            revision = changes.revision
            receipt = try await snapshotClient.requestStatus(requestID: requestID)
            revision = max(revision, receipt.revision)
        }
    }

    private func checkReadDeadline(_ deadline: ContinuousClock.Instant) throws {
        try Task.checkCancellation()
        guard active else { throw CancellationError() }
        guard ContinuousClock().now < deadline else { throw RelaySessionError.outcomeUnknown }
    }

    private func readUntilChange(requestID: String, path: String, deadline: ContinuousClock.Instant) async throws -> RelayOperationResult {
        let clock = ContinuousClock()
        let bootstrap = try await client.bootstrap()
        try checkReadDeadline(deadline)
        let envelope = try sealRequest(requestID: requestID, method: "GET", path: path, body: nil)
        try checkReadDeadline(deadline)
        do {
            let receipt: RelayReceipt
            do {
                receipt = try await client.submitRequest(envelope)
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
        catch { return .unknown }
    }

}
