import CryptoKit
import Foundation

/// Sealed requests to one paired Mac over its direct port, answered in the same exchange.
/// A mutation is sealed once per operation ID and only ever resent as the same bytes: the Mac's
/// receipt journal runs it once and replays the stored answer.
public actor DirectSession: OperationAPI {
    private let codec: EnvelopeCodec
    private let router: DirectRouter
    private let http: DirectHTTP
    private let pause: @Sendable (Duration) async throws -> Void
    /// Most recent operations first; old ones age out, their envelopes long expired anyway.
    private var sealed: [String: (tag: Data, body: Data)] = [:]
    private var sealedOrder: [String] = []
    static let maxSealed = 256
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
            sealedOrder.append(request.operationID)
            if sealedOrder.count > Self.maxSealed { sealed[sealedOrder.removeFirst()] = nil }
        }
        return try await deliver(body, requestID: request.operationID, mutation: true, timeout: timeout)
    }

    /// Takes over an operation another carrier already sealed and sent: resending those exact
    /// bytes lets the Mac's journal answer with what it stored instead of running it again.
    public func adopt(_ envelope: Envelope, for request: APIRequest) throws {
        guard sealed[request.operationID] == nil, envelope.requestID == request.operationID else { return }
        let tag = try codec.semanticTag(method: request.method, path: request.path, body: request.body)
        sealed[request.operationID] = (tag, try JSONEncoder().encode(envelope))
        sealedOrder.append(request.operationID)
        if sealedOrder.count > Self.maxSealed { sealed[sealedOrder.removeFirst()] = nil }
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

    /// Up to four tries, re-picking the route after a network failure or an impostor. A mutation that may have
    /// reached the Mac without an answer is `.unknown`, never silently dropped.
    private func deliver(_ body: Data, requestID: String, mutation: Bool, timeout: Duration) async throws -> APIResponse {
        var mayHaveArrived = false
        for attempt in 0..<4 {
            guard active else { throw CancellationError() }
            if attempt > 0 { try await pause(.milliseconds(500 * attempt)) }
            let endpoint: DirectEndpoint
            do { endpoint = try await router.endpoint() }
            catch is DirectRouteError where mayHaveArrived { break }
            let reply: HTTPReply
            do { reply = try await http.post(endpoint, path: "/direct/request", body: body, timeout: timeout) }
            catch let error as URLError where error.code == .cancelled { throw CancellationError() }
            catch is URLError {
                mayHaveArrived = true
                await router.invalidate()
                continue
            }
            guard active else { throw CancellationError() }
            if reply.status == 200 {
                // Only an answer that opens with the pinned keys proves the address is this Mac.
                if let envelope = try? JSONDecoder().decode(Envelope.self, from: reply.body),
                   let response = try? codec.opened(envelope, requestID: requestID) {
                    await router.proved(endpoint)
                    if Self.saysRevoked(response) { throw DirectSessionError.deviceRevoked }
                    return response
                }
                await router.reject(endpoint)
                continue
            }
            // A bare status is not signed, so it never says anything about the Mac or this
            // device: a refusal rules out only that address, and the Mac stays as not answering.
            // From an address proved on this network, a conflict or a rejection is believed.
            guard reply.status != 401, await router.isProven(endpoint) else {
                await router.reject(endpoint)
                continue
            }
            switch reply.status {
            case 409: throw RelaySessionError.operationIdentityConflict
            case 421: await router.reject(endpoint)
            case 429, 503: continue
            case 400: return .completed(status: 400, body: reply.body)
            default: await router.reject(endpoint)
            }
        }
        if mutation && mayHaveArrived { return .unknown }
        throw DirectRouteError.unreachable
    }
}
