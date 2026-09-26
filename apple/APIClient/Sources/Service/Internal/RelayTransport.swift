import CryptoKit
import Foundation
import Security

/// Transport for the opaque relay protocol. Pairing and Keychain ownership belong to the caller.
/// POST /v1/requests is issued once. The caller must persist its encoded envelope before sending,
/// then resolve an ambiguous network failure with GET /v1/requests/{id}.
actor RelayTransport: ServiceTransport {
    private let base: URL
    private let deviceID: String
    private let signer: @Sendable (Data) throws -> Data
    private let bearer: @Sendable () async throws -> String
    private let transport: any ServiceHTTPTransport
    private var epoch: Data?
    private let now: @Sendable () -> UInt64

    public init(base: URL, deviceID: String,
                signer: @escaping @Sendable (Data) throws -> Data,
                bearer: @escaping @Sendable () async throws -> String,
                transport: any ServiceHTTPTransport,
                now: @escaping @Sendable () -> UInt64) throws {
        let scheme = base.scheme?.lowercased()
        guard scheme == "https",
              base.user == nil, base.password == nil, base.query == nil, base.fragment == nil,
              base.path.isEmpty || base.path == "/", RelayHTTPChallenge.validText(deviceID)
        else { throw RelayWireError.invalidEndpoint }
        self.base = base; self.deviceID = deviceID; self.signer = signer
        self.bearer = bearer; self.transport = transport; self.now = now
    }

    public func bootstrap() async throws -> RelayBootstrap {
        let result: RelayBootstrap = try await get("/v1/bootstrap", signed: false)
        guard result.protocolVersion == 1, try Base64URL.decode(result.epoch).count == 16 else {
            throw RelayWireError.invalidResponse
        }
        epoch = try Base64URL.decode(result.epoch)
        return result
    }

    public func health() async throws -> Bool {
        let response = try await send("GET", target: "/healthz", body: Data(), signed: false)
        guard response.status == 200 else { throw Self.error(response.status) }
        struct Health: Decodable { let ok: Bool }
        return try Self.decoder.decode(Health.self, from: response.body).ok
    }

    public func submit(_ envelope: EnvelopeV1) async throws -> RelayJob {
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

    public func status(requestID: String) async throws -> RelayJob {
        guard Self.validRequestID(requestID) else { throw RelayWireError.invalidField }
        let job: RelayJob = try await get("/v1/requests/\(requestID)", signed: true)
        guard job.requestID == requestID else { throw RelayWireError.invalidResponse }
        return job
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

    func pairingRequest(method: String, target: String, body: Data) async throws -> ServiceHTTPResponse {
        guard target.hasPrefix("/v1/pairings/"), ["GET", "POST"].contains(method) else { throw RelayWireError.invalidEndpoint }
        return try await send(method, target: target, body: body, signed: true)
    }

    private func get<T: Decodable>(_ target: String, signed: Bool) async throws -> T {
        let response = try await send("GET", target: target, body: Data(), signed: signed)
        guard response.status == 200 else { throw Self.error(response.status) }
        return try Self.decoder.decode(T.self, from: response.body)
    }

    private func send(_ method: String, target: String, body: Data, signed: Bool) async throws -> ServiceHTTPResponse {
        guard target.hasPrefix("/"), !target.hasPrefix("//"), !target.contains("#"),
              let url = URL(string: target, relativeTo: base)?.absoluteURL,
              url.host == base.host, url.scheme == base.scheme else { throw RelayWireError.invalidEndpoint }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = target.hasPrefix("/v1/changes?") ? 35 : 15
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if method == "POST" {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if signed {
            if epoch == nil { _ = try await bootstrap() }
            guard let epoch else { throw RelayWireError.invalidResponse }
            let token = try await bearer()
            guard !token.isEmpty, !token.utf8.contains(where: { $0 <= 32 || $0 == 127 }) else {
                throw RelayWireError.invalidField
            }
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
        return try await transport.send(request)
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

