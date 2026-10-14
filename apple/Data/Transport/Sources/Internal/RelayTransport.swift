import CryptoKit
import Foundation
import Security

/// Transport for the opaque relay protocol. Pairing and Keychain ownership belong to the caller.
/// Persist the encoded envelope before dispatch. An explicit admission refusal permits retrying
/// those same bytes; an ambiguous network failure permits only GET /v1/requests/{id}.
actor RelayTransport: Transport {
    private let base: URL
    private let deviceID: String
    private let signer: @Sendable (Data) throws -> Data
    private let bearer: @Sendable () async throws -> String
    private let bearerRefused: @Sendable () async -> Void
    private let bearerRefusedForToken: (@Sendable (String) async -> Void)?
    private let transport: any HTTPTransport
    private struct BootstrapFlight {
        let id: UUID
        let generation: UInt64
        let proofGeneration: UInt64
        let task: Task<Void, Never>
    }
    private var bootstrapFlight: BootstrapFlight?
    private var bootstrapWaiters: [UUID: CheckedContinuation<RelayBootstrap, any Error>] = [:]
    private var bootstrapGeneration: UInt64 = 0
    private var bootstrapValue: RelayBootstrap?
    private var bootstrapObserver: (@Sendable (Int) -> Void)?
    private var epoch: Data?
    /// A signed request has been accepted with the current epoch, so the relay knew it.
    private var epochProven = false
    private var proofGeneration: UInt64 = 0
    private var latestBearer: String?
    private let now: @Sendable () -> UInt64

    public init(base: URL, deviceID: String,
                signer: @escaping @Sendable (Data) throws -> Data,
                bearer: @escaping @Sendable () async throws -> String,
                bearerRefused: @escaping @Sendable () async -> Void = {},
                bearerRefusedForToken: (@Sendable (String) async -> Void)? = nil,
                transport: any HTTPTransport,
                now: @escaping @Sendable () -> UInt64) throws {
        let scheme = base.scheme?.lowercased()
        guard scheme == "https",
              base.user == nil, base.password == nil, base.query == nil, base.fragment == nil,
              base.path.isEmpty || base.path == "/", RelayHTTPChallenge.validText(deviceID)
        else { throw RelayWireError.invalidEndpoint }
        self.base = base; self.deviceID = deviceID; self.signer = signer
        self.bearer = bearer; self.bearerRefused = bearerRefused; self.bearerRefusedForToken = bearerRefusedForToken; self.transport = transport; self.now = now
    }

    public func bootstrap() async throws -> RelayBootstrap {
        try await waitForBootstrap()
    }

    public func bootstrapIfNeeded() async throws -> RelayBootstrap {
        try Task.checkCancellation()
        try MutationScope.check()
        if let bootstrapValue { return bootstrapValue }
        return try await waitForBootstrap()
    }

    var bootstrapHTTPTask: Task<Void, Never>? { bootstrapFlight?.task }

    func observeBootstrapWaiters(_ observer: (@Sendable (Int) -> Void)?) {
        bootstrapObserver = observer
        observer?(bootstrapWaiters.count)
    }

    private func waitForBootstrap() async throws -> RelayBootstrap {
        let id = UUID()
        let result = try await withTaskCancellationHandler {
            try Task.checkCancellation()
            try MutationScope.check()
            return try await withCheckedThrowingContinuation { continuation in
                bootstrapWaiters[id] = continuation
                bootstrapObserver?(bootstrapWaiters.count)
                admitBootstrap()
            }
        } onCancel: {
            Task { await self.cancelBootstrapWaiter(id) }
        }
        try Task.checkCancellation()
        try MutationScope.check()
        return result
    }

    private func cancelBootstrapWaiter(_ id: UUID) {
        guard let waiter = bootstrapWaiters.removeValue(forKey: id) else { return }
        bootstrapObserver?(bootstrapWaiters.count)
        waiter.resume(throwing: CancellationError())
        if bootstrapWaiters.isEmpty {
            bootstrapGeneration &+= 1
            bootstrapFlight?.task.cancel()
        }
    }

    func cancelBootstrap() {
        bootstrapGeneration &+= 1
        let waiters = bootstrapWaiters
        bootstrapWaiters.removeAll()
        bootstrapObserver?(0)
        for waiter in waiters.values { waiter.resume(throwing: CancellationError()) }
        bootstrapFlight?.task.cancel()
    }

    private func invalidateBootstrap() {
        epoch = nil
        epochProven = false
        bootstrapValue = nil
        bootstrapGeneration &+= 1
        bootstrapFlight?.task.cancel()
    }

    private func admitBootstrap() {
        guard bootstrapFlight == nil, !bootstrapWaiters.isEmpty else { return }
        let id = UUID(), generation = bootstrapGeneration
        let http = transport
        var request = URLRequest(url: URL(string: "/v1/bootstrap", relativeTo: base)!.absoluteURL)
        request.httpMethod = "GET"
        request.timeoutInterval = 15
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        // One owned HTTP request outlives any individual caller's budget or cancellation.
        let task = MutationScope.$current.withValue(nil) {
            Task { @concurrent [weak self, request] in
                let result: Result<RelayBootstrap, any Error>
                do {
                    try Task.checkCancellation()
                    let response = try await http.send(request)
                    guard response.status == 200 else { throw Self.error(response.status) }
                    let value = try JSONDecoder().decode(RelayBootstrap.self, from: response.body)
                    guard value.protocolVersion == 1, try Base64URL.decode(value.epoch).count == 16 else {
                        throw RelayWireError.invalidResponse
                    }
                    result = .success(value)
                } catch { result = .failure(error) }
                await self?.finishBootstrap(id: id, generation: generation, result: result)
            }
        }
        bootstrapFlight = BootstrapFlight(id: id, generation: generation, proofGeneration: proofGeneration, task: task)
    }

    private func finishBootstrap(id: UUID, generation: UInt64, result: Result<RelayBootstrap, any Error>) {
        guard let flight = bootstrapFlight, flight.id == id else { return }
        bootstrapFlight = nil
        guard generation == bootstrapGeneration else { admitBootstrap(); return }
        let waiters = bootstrapWaiters
        bootstrapWaiters.removeAll()
        bootstrapObserver?(0)
        if !waiters.isEmpty, case let .success(value) = result {
            let freshEpoch = try! Base64URL.decode(value.epoch)
            if epoch != freshEpoch || flight.proofGeneration == proofGeneration { epochProven = false }
            epoch = freshEpoch
            bootstrapValue = value
        }
        for waiter in waiters.values { waiter.resume(with: result) }
    }

    public func health() async throws -> Bool {
        let response = try await send("GET", target: "/healthz", body: Data(), signed: false)
        guard response.status == 200 else { throw Self.error(response.status) }
        struct Health: Decodable { let ok: Bool }
        return try Self.decoder.decode(Health.self, from: response.body).ok
    }

    public func submit(_ envelope: Envelope) async throws -> RelayJob {
        guard envelope.kind == "request", envelope.sender == deviceID, envelope.route == envelope.recipient else {
            throw RelayWireError.invalidField
        }
        _ = try envelope.signedBytes()
        let body = try Self.encoder.encode(envelope)
        guard body.count <= 256 * 1024 else { throw RelayWireError.invalidField }
        let response = try await send("POST", target: "/v1/requests", body: body, signed: true)
        guard response.status == 200 || response.status == 202 else { throw Self.error(response.status) }
        let job = try Self.decoder.decode(RelayJob.self, from: response.body)
        guard job.requestID == envelope.requestID else { throw RelayWireError.invalidResponse }
        return job
    }

    public func acknowledgeResult(requestID: String, revision: UInt64) async throws {
        guard Self.validRequestID(requestID) else { throw RelayWireError.invalidField }
        guard bootstrapValue?.resultAcknowledgment == true else { return }
        let body = try Self.encoder.encode(["revision": revision])
        let response = try await send("POST", target: "/v1/requests/\(requestID)/ack", body: body, signed: true)
        guard response.status == 204 else { throw Self.error(response.status) }
    }

    public func status(requestID: String) async throws -> RelayJob {
        guard Self.validRequestID(requestID) else { throw RelayWireError.invalidField }
        let job: RelayJob = try await get("/v1/requests/\(requestID)", signed: true)
        guard job.requestID == requestID else { throw RelayWireError.invalidResponse }
        return job
    }

    public func waitForRequest(requestID: String, waitSeconds: UInt64) async throws -> RelayReceipt {
        guard Self.validRequestID(requestID), (1...25).contains(waitSeconds) else { throw RelayWireError.invalidField }
        let job: RelayJob = try await get("/v1/requests/\(requestID)?wait_s=\(waitSeconds)", signed: true)
        guard job.requestID == requestID else { throw RelayWireError.invalidResponse }
        return RelayReceipt(requestID: job.requestID, status: job.status, revision: job.revision, response: job.response)
    }

    public func changes(since: UInt64, epoch expectedEpoch: String? = nil, waitSeconds: UInt64 = 25) async throws -> RelayChanges {
        guard waitSeconds <= 25 else { throw RelayWireError.invalidField }
        var target = "/v1/changes?since=\(since)"
        if let expectedEpoch {
            guard try Base64URL.decode(expectedEpoch).count == 16 else { throw RelayWireError.invalidField }
            target += "&epoch=\(expectedEpoch)"
        }
        target += "&wait_s=\(waitSeconds)"
        return try await get(target, signed: true)
    }

    public func registerPush(_ registration: RelayPushRegistration) async throws {
        let response = try await send("POST", target: "/v1/push/register",
                                      body: Self.encoder.encode(registration), signed: true)
        guard response.status == 204 else { throw Self.error(response.status) }
    }

    func pairingRequest(method: String, target: String, body: Data) async throws -> HTTPReply {
        guard target.hasPrefix("/v1/pairings/"), ["GET", "POST"].contains(method) else { throw RelayWireError.invalidEndpoint }
        return try await send(method, target: target, body: body, signed: true)
    }

    private func get<T: Decodable>(_ target: String, signed: Bool) async throws -> T {
        let response = try await send("GET", target: target, body: Data(), signed: signed)
        guard response.status == 200 else { throw Self.error(response.status) }
        return try Self.decoder.decode(T.self, from: response.body)
    }

    /// A relay restarted since the epoch was signed refused the request before reading it, so it
    /// is signed again for the new epoch once.
    private func send(_ method: String, target: String, body: Data, signed: Bool) async throws -> HTTPReply {
        let reply = try await sendOnce(method, target: target, body: body, signed: signed)
        guard signed, Self.epochChanged(reply) else { return reply }
        return try await sendOnce(method, target: target, body: body, signed: signed)
    }

    private static func epochChanged(_ reply: HTTPReply) -> Bool {
        reply.status == 401 && (try? decoder.decode([String: String].self, from: reply.body))?["error"] == "relay_epoch_changed"
    }

    private func sendOnce(_ method: String, target: String, body: Data, signed: Bool) async throws -> HTTPReply {
        try Task.checkCancellation()
        guard target.hasPrefix("/"), !target.hasPrefix("//"), !target.contains("#"),
              let url = URL(string: target, relativeTo: base)?.absoluteURL,
              url.host == base.host, url.scheme == base.scheme else { throw RelayWireError.invalidEndpoint }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = target.hasPrefix("/v1/changes?") || target.hasPrefix("/v1/requests/") && target.contains("?wait_s=") ? 35 : 15
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if method == "POST" {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        var signedEpoch: Data?
        var signedBearer: String?
        if signed {
            try MutationScope.check()
            if epoch == nil { _ = try await bootstrapIfNeeded() }
            try MutationScope.check()
            guard let epoch else { throw RelayWireError.invalidResponse }
            let token = try await MutationScope.$current.withValue(nil) { try await bearer() }
            try Task.checkCancellation()
            try MutationScope.check()
            guard !token.isEmpty, !token.utf8.contains(where: { $0 <= 32 || $0 == 127 }) else {
                throw RelayWireError.invalidField
            }
            signedEpoch = epoch
            signedBearer = token
            latestBearer = token
            let timestamp = now()
            let random = try Self.randomNonce()
            let challenge = try RelayHTTPChallenge.message(deviceID: deviceID, method: method,
                                                           target: target, epoch: epoch, body: body,
                                                           timestampMilliseconds: timestamp, nonce: random)
            let signature = try signer(challenge)
            guard signature.count == 64 else { throw RelayWireError.invalidSignature }
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            request.setValue(deviceID, forHTTPHeaderField: "x-device-id")
            request.setValue(Base64URL.encode(epoch), forHTTPHeaderField: "x-relay-epoch")
            request.setValue(String(timestamp), forHTTPHeaderField: "x-device-timestamp-ms")
            request.setValue(Base64URL.encode(random), forHTTPHeaderField: "x-device-nonce")
            request.setValue(Base64URL.encode(signature), forHTTPHeaderField: "x-device-signature")
        }
        request.timeoutInterval = try MutationScope.remaining(cap: .seconds(request.timeoutInterval)).requestTimeInterval
        try Task.checkCancellation()
        try MutationScope.check()
        if method == "POST", target == "/v1/requests" { try MutationScope.possibleDispatch() }
        let reply = try await transport.send(request)
        if Task.isCancelled || MutationScope.current?.interruption != nil { return reply }
        guard signed, signedEpoch == epoch, signedBearer == latestBearer else { return reply }
        let refusal = try? JSONDecoder().decode([String: String].self, from: reply.body)
        let admissionRefused = method == "POST" && target == "/v1/requests"
            && (reply.status == 429 && refusal == ["error": "queue_full"]
                || reply.status == 503 && refusal == ["error": "server_busy"])
        if reply.status == 401 {
            // A relay restart changes its epoch, and an epoch that already worked is the likely
            // cause: bootstrap again first. A 401 with a fresh epoch is about the access token,
            // never that the Mac revoked this device: renew it before the next request.
            if epochProven || Self.epochChanged(reply) { invalidateBootstrap() }
            else {
                await MutationScope.$current.withValue(nil) {
                    if let bearerRefusedForToken, let signedBearer { await bearerRefusedForToken(signedBearer) }
                    else { await bearerRefused() }
                }
            }
        } else if reply.status < 500 {
            epochProven = true
            proofGeneration &+= 1
        }
        if admissionRefused { throw RelayWireError.admissionFull }
        return reply
    }

    private static let encoder: JSONEncoder = {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]; return encoder
    }()
    private static let decoder = JSONDecoder()

    private static func validRequestID(_ id: String) -> Bool {
        !id.isEmpty && id.utf8.count <= 128 && id.utf8.allSatisfy {
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
        }
    }

    private static func error(_ status: Int) -> RelayWireError {
        switch status {
        case 401: return .unauthorized
        case 409: return .conflict
        case 410: return .outcomeUnknown
        default: return .server(status)
        }
    }

    private static func randomNonce() throws -> Data {
        var bytes = [UInt8](repeating: 0, count: 16)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw RelayWireError.invalidEncoding
        }
        return Data(bytes)
    }
}

extension RelayTransport {
    public func submitRequest(_ envelope: Envelope) async throws -> RelayReceipt {
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
