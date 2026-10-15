import CryptoKit
import Foundation
import Synchronization

final class DirectSessionLifetime: Sendable {
    private let open = Mutex(true)

    var isActive: Bool { open.withLock { $0 } }

    func deactivate() { open.withLock { $0 = false } }

    func whileActive(_ update: () -> Void) {
        open.withLock { active in
            if active { update() }
        }
    }
}

/// Sealed requests to one paired Mac, answered in the same exchange: over its direct port, or
/// over its iroh endpoint.
/// A mutation is sealed once per operation ID and only ever resent as the same bytes: the Mac's
/// receipt journal runs it once and replays the stored answer.
public actor DirectSession: OperationAPI {
    private let journal: EnvelopeJournal?
    private let codec: EnvelopeCodec
    private let carrier: any EnvelopeCarrier
    private let quickSlack: Duration
    private let requestClock: RequestClock
    let mutations: MutationRunner
    private let mutationEvidence = MutationEvidenceStore()
    /// Most recent operations first; old ones age out, their envelopes long expired anyway.
    private var sealed: [String: (tag: Data, body: Data)] = [:]
    private var sealedOrder: [String] = []
    static let maxSealed = 256
    private let lifetime = DirectSessionLifetime()
    private var active: Bool { lifetime.isActive }

    public init(macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
                agreement: P256.KeyAgreement.PrivateKey, macSigning: P256.Signing.PublicKey,
                macAgreement: P256.KeyAgreement.PublicKey, router: DirectRouter, http: DirectHTTP,
                journal: EnvelopeJournal? = nil,
                now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) },
                pause: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        self.init(macID: macID, deviceID: deviceID, signing: signing, agreement: agreement,
            macSigning: macSigning, macAgreement: macAgreement, router: router, http: http,
            journal: journal, now: now, pause: pause, requestClock: .continuous)
    }

    init(macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
         agreement: P256.KeyAgreement.PrivateKey, macSigning: P256.Signing.PublicKey,
         macAgreement: P256.KeyAgreement.PublicKey, router: DirectRouter, http: DirectHTTP,
         journal: EnvelopeJournal? = nil,
         now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) },
         pause: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
         requestClock: RequestClock) {
        self.init(macID: macID, deviceID: deviceID, signing: signing, agreement: agreement,
            macSigning: macSigning, macAgreement: macAgreement,
            carrier: HTTPDirectCarrier(router: router, http: http, pause: pause),
            journal: journal, now: now, requestClock: requestClock)
    }

    /// The same sealed requests over a Mac's iroh endpoint.
    public init(macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
                agreement: P256.KeyAgreement.PrivateKey, macSigning: P256.Signing.PublicKey,
                macAgreement: P256.KeyAgreement.PublicKey, iroh: IrohCarrier, journal: EnvelopeJournal? = nil,
                now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }) {
        self.init(macID: macID, deviceID: deviceID, signing: signing, agreement: agreement,
            macSigning: macSigning, macAgreement: macAgreement, carrier: iroh,
            journal: journal, now: now, requestClock: .continuous)
    }

    init(macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
         agreement: P256.KeyAgreement.PrivateKey, macSigning: P256.Signing.PublicKey,
         macAgreement: P256.KeyAgreement.PublicKey, carrier: any EnvelopeCarrier,
         journal: EnvelopeJournal?, now: @escaping @Sendable () -> UInt64, requestClock: RequestClock,
         quickSlack: Duration = .seconds(DirectSession.quickReadSlack)) {
        self.quickSlack = quickSlack
        self.journal = journal
        self.requestClock = requestClock
        mutations = MutationRunner(clock: requestClock)
        codec = EnvelopeCodec(macID: macID, deviceID: deviceID, signing: signing, agreement: agreement,
                              macSigning: macSigning, macAgreement: macAgreement, now: now)
        self.carrier = carrier
    }

    public func recover(_ request: APIRequest) async throws -> APIResponse {
        try await lookup(request).response
    }

    public func lookup(_ request: APIRequest) async throws -> OperationLookup {
        guard request.method == "POST", request.path == "/actions", active, let journal, let entry = try await journal.entry(requestID: request.operationID),
              entry.phase != .retired, let envelope = entry.envelope else { return .unknown }
        guard entry.semanticTag == (try codec.semanticTag(method: request.method, path: request.path, body: request.body)) else {
            throw RelaySessionError.operationIdentityConflict
        }
        return try await codec.lookup(request, envelope: envelope) { try await self.perform($0) }
    }

    public func resend(_ request: APIRequest) async throws -> APIResponse {
        try await resend(request, adopting: nil)
    }

    /// The same as `resend`, for an action another carrier sealed and sent as `envelope`.
    public func resend(_ request: APIRequest, adopting envelope: Envelope?) async throws -> APIResponse {
        guard request.method == "POST", request.path == "/actions", active, let journal else { return .unknown }
        let entry = try await journal.entry(requestID: request.operationID)
        guard entry?.phase != .retired, let original = entry?.envelope ?? envelope else { return .unknown }
        if let entry, entry.semanticTag != (try codec.semanticTag(method: request.method, path: request.path, body: request.body)) {
            throw RelaySessionError.operationIdentityConflict
        }
        guard codec.resendable(original) else { throw OperationError.sealExpired }
        try await adopt(original, for: request)
        return try await perform(request)
    }

    public func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        try await journal?.retire(acknowledgment)
        sealed = sealed.filter { !acknowledgment.retires($0.key) }
        sealedOrder.removeAll { acknowledgment.retires($0) }
        mutationEvidence.retireActions(acknowledgment)
    }

    public func deactivate() async {
        lifetime.deactivate()
        await mutations.cancelAll()
    }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        guard request.method != "GET", !request.isActionJournalControl else { return try await performPrepared(request) }
        let startedAt = requestClock.now()
        let owner = MutationScope.current?.id ?? UUID()
        let lease = mutationEvidence.acquire(request.operationID, owner: owner)
        defer { mutationEvidence.release(lease) }
        let result = try await mutations.run(dispatchEvidence: lease.evidence, ownerID: owner, startedAt: startedAt, unknown: { APIResponse.unknown }) {
            try await self.performPrepared(request)
        }
        return result
    }

    /// One short try at a read, for a caller with another carrier to fall back on: a slow or
    /// silent Mac hands the read over in seconds instead of after four full timeouts.
    public func quickRead(_ request: APIRequest) async throws -> APIResponse {
        guard request.method == "GET", !request.isActionJournalControl else { return try await perform(request) }
        return try await performPrepared(request, pace: .quick)
    }

    /// One try at a read with the full time to answer, for a carrier that knows at once when the
    /// Mac is gone: a Mac that is only slow keeps its answer instead of rendering it twice.
    public func liveRead(_ request: APIRequest) async throws -> APIResponse {
        guard request.method == "GET", !request.isActionJournalControl else { return try await perform(request) }
        return try await performPrepared(request, pace: .live)
    }

    static let quickReadSlack = 8

    private enum ReadPace { case patient, quick, live }

    private func performPrepared(_ request: APIRequest, pace: ReadPace = .patient) async throws -> APIResponse {
        try MutationScope.check()
        guard active else { throw CancellationError() }
        let wait = EnvelopeRequestPolicy.viewsChangeWait(request.path)
        guard ["GET", "POST", "PUT"].contains(request.method), !request.path.contains("#"),
              wait != nil || EnvelopeRequestPolicy.validViewsPath(method: request.method, path: request.path,
                                                                  hasBody: request.body != nil)
        else { throw RelaySessionError.invalidRequest }
        let timeout = .seconds(wait ?? 0) + (pace == .quick ? quickSlack : .seconds(20))
        if request.isActionJournalControl {
            let requestID = "control-" + UUID().uuidString
            let body = try seal(requestID: requestID, method: request.method, path: request.path, body: request.body)
            return try await deliver(body, requestID: requestID, mutation: false, timeout: timeout)
        }
        if request.method == "GET" {
            let requestID = "read-" + UUID().uuidString
            let body = try seal(requestID: requestID, method: "GET", path: request.path, body: nil)
            return try await deliver(body, requestID: requestID, mutation: false, timeout: timeout, attempts: pace == .patient ? 4 : 1)
        }
        guard !request.operationID.hasPrefix("read-") else { throw RelaySessionError.invalidRequest }
        let tag = try codec.semanticTag(method: request.method, path: request.path, body: request.body)
        let body: Data
        let durableAction = request.method == "POST" && request.path == "/actions"
        if durableAction, let journal, let existing = try await journal.entry(requestID: request.operationID) {
            guard existing.semanticTag == tag else { throw RelaySessionError.operationIdentityConflict }
            if let live = sealed[request.operationID] {
                guard let original = existing.envelope,
                      try JSONDecoder().decode(Envelope.self, from: live.body) == original else {
                    throw RelaySessionError.operationIdentityConflict
                }
            } else { return try await recover(request) }
        }
        if let existing = sealed[request.operationID] {
            guard existing.tag == tag else { throw RelaySessionError.operationIdentityConflict }
            body = existing.body
        } else {
            body = try seal(requestID: request.operationID, method: request.method, path: request.path, body: request.body)
            try MutationScope.check()
            if durableAction, let journal {
                try await journal.reserve(RelayOutboxEntry(requestID: request.operationID, scope: journal.scope,
                    envelope: JSONDecoder().decode(Envelope.self, from: body), phase: .reserved, semanticTag: tag))
            }
            sealed[request.operationID] = (tag, body)
            sealedOrder.append(request.operationID)
            if sealedOrder.count > Self.maxSealed {
                let removed = sealedOrder.removeFirst()
                sealed[removed] = nil
                mutationEvidence.forget(removed)
            }
        }
        mutationEvidence.retain(request.operationID)
        return try await deliver(body, requestID: request.operationID, mutation: true, timeout: timeout, persistResult: durableAction && journal != nil)
    }

    /// Takes over an operation another carrier already sealed and sent: resending those exact
    /// bytes lets the Mac's journal answer with what it stored instead of running it again.
    public func adopt(_ envelope: Envelope, for request: APIRequest) async throws {
        try MutationScope.check()
        guard sealed[request.operationID] == nil, envelope.requestID == request.operationID else { return }
        let tag = try codec.semanticTag(method: request.method, path: request.path, body: request.body)
        if request.method == "POST", request.path == "/actions", let journal {
            if let existing = try await journal.entry(requestID: request.operationID) {
                guard existing.semanticTag == tag, existing.envelope == envelope else {
                    throw RelaySessionError.operationIdentityConflict
                }
            } else {
                try await journal.reserve(RelayOutboxEntry(requestID: request.operationID, scope: journal.scope,
                    envelope: envelope, phase: .reserved, semanticTag: tag))
            }
        }
        sealed[request.operationID] = (tag, try JSONEncoder().encode(envelope))
        sealedOrder.append(request.operationID)
        if sealedOrder.count > Self.maxSealed {
                let removed = sealedOrder.removeFirst()
                sealed[removed] = nil
                mutationEvidence.forget(removed)
            }
    }

    private func seal(requestID: String, method: String, path: String, body: Data?) throws -> Data {
        do {
            let envelope = try codec.sealRequest(requestID: requestID, method: method, path: path, body: body)
            return try JSONEncoder().encode(envelope)
        } catch EnvelopeError.invalidRequest { throw RelaySessionError.invalidRequest }
    }

    /// The Mac's sealed answer to a device it revoked.
    private static func saysRevoked(_ response: APIResponse) -> Bool {
        guard case let .completed(status, body) = response, status == 403,
              let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] else { return false }
        return json["error"] as? String == "device_revoked"
    }

    private func deliver(_ body: Data, requestID: String, mutation: Bool, timeout: Duration, attempts: Int = 4,
                         persistResult: Bool = false) async throws -> APIResponse {
        let codec = codec
        let answer = try await carrier.deliver(body, open: { try codec.opened($0, requestID: requestID) },
            mutation: mutation, timeout: timeout, attempts: attempts, lifetime: lifetime)
        switch answer {
        case .unknown: return .unknown
        case let .plain(response): return response
        case let .sealed(envelope, response):
            if persistResult, let journal {
                _ = try await journal.update(requestID: requestID) { record in
                    guard record.phase != .retired else { return }
                    record.phase = .acknowledged
                    record.lastStatus = "completed"
                    record.lastResponse = envelope
                }
            }
            if Self.saysRevoked(response) { throw DirectSessionError.deviceRevoked }
            return response
        }
    }
}
