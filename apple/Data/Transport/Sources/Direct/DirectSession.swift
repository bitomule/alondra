import CryptoKit
import Foundation

public enum DirectSessionError: Error, Equatable, Sendable {
    /// The Mac no longer accepts this device: revoked, never approved, or a stale envelope.
    case unauthorized
}

/// Sealed requests to one paired Mac over its direct port, answered in the same exchange.
/// A mutation is sealed once per operation ID and only ever resent as the same bytes: the Mac's
/// receipt journal runs it once and replays the stored answer.
public actor DirectSession: OperationAPI {
    private let codec: EnvelopeCodec
    private let router: DirectRouter
    private let http: DirectHTTP
    private let pause: @Sendable (Duration) async throws -> Void
    private var sealed: [String: (tag: Data, body: Data)] = [:]
    private var active = true

    public init(macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
                agreement: P256.KeyAgreement.PrivateKey, macSigning: P256.Signing.PublicKey,
                macAgreement: P256.KeyAgreement.PublicKey, router: DirectRouter, http: DirectHTTP,
                now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) },
                pause: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        codec = EnvelopeCodec(macID: macID, deviceID: deviceID, signing: signing, agreement: agreement,
                              macSigning: macSigning, macAgreement: macAgreement, now: now)
        self.router = router
        self.http = http
        self.pause = pause
    }

    public func deactivate() { active = false }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        guard active else { throw CancellationError() }
        let wait = EnvelopeRequestPolicy.viewsChangeWait(request.path)
        guard ["GET", "POST"].contains(request.method), !request.path.contains("#"),
              wait != nil || EnvelopeRequestPolicy.validViewsPath(method: request.method, path: request.path,
                                                                  hasBody: request.body != nil)
        else { throw RelaySessionError.invalidRequest }
        let timeout = Duration.seconds((wait ?? 0) + 20)
        if request.method == "GET" {
            let requestID = "read-" + UUID().uuidString
            let body = try seal(requestID: requestID, method: "GET", path: request.path, body: nil)
            return try await deliver(body, requestID: requestID, mutation: false, timeout: timeout)
        }
        guard !request.operationID.hasPrefix("read-") else { throw RelaySessionError.invalidRequest }
        let tag = try codec.semanticTag(method: request.method, path: request.path, body: request.body)
        let body: Data
        if let existing = sealed[request.operationID] {
            guard existing.tag == tag else { throw RelaySessionError.operationIdentityConflict }
            body = existing.body
        } else {
            body = try seal(requestID: request.operationID, method: request.method, path: request.path, body: request.body)
            sealed[request.operationID] = (tag, body)
        }
        return try await deliver(body, requestID: request.operationID, mutation: true, timeout: timeout)
    }

    private func seal(requestID: String, method: String, path: String, body: Data?) throws -> Data {
        do {
            let envelope = try codec.sealRequest(requestID: requestID, method: method, path: path, body: body)
            return try JSONEncoder().encode(envelope)
        } catch EnvelopeError.invalidRequest { throw RelaySessionError.invalidRequest }
    }

    /// Up to three tries, re-picking the route after a network failure. A mutation that may have
    /// reached the Mac without an answer is `.unknown`, never silently dropped.
    private func deliver(_ body: Data, requestID: String, mutation: Bool, timeout: Duration) async throws -> APIResponse {
        var mayHaveArrived = false
        for attempt in 0..<3 {
            guard active else { throw CancellationError() }
            if attempt > 0 { try await pause(.milliseconds(500 * attempt)) }
            let endpoint: DirectEndpoint
            do { endpoint = try await router.endpoint() }
            catch DirectRouteError.unreachable where mayHaveArrived { break }
            let reply: HTTPReply
            do { reply = try await http.post(endpoint, path: "/direct/request", body: body, timeout: timeout) }
            catch let error as URLError where error.code == .cancelled { throw CancellationError() }
            catch is URLError {
                mayHaveArrived = true
                await router.invalidate()
                continue
            }
            guard active else { throw CancellationError() }
            switch reply.status {
            case 200:
                guard let envelope = try? JSONDecoder().decode(Envelope.self, from: reply.body) else {
                    throw RelayWireError.invalidResponse
                }
                return try codec.opened(envelope, requestID: requestID)
            case 401: throw DirectSessionError.unauthorized
            case 409: throw RelaySessionError.operationIdentityConflict
            case 421: await router.invalidate()
            case 429, 503: continue
            case 400: return .completed(status: 400, body: reply.body)
            default: throw RelayWireError.server(reply.status)
            }
        }
        if mutation && mayHaveArrived { return .unknown }
        throw DirectRouteError.unreachable
    }
}
