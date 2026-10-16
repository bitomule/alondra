import CryptoKit
import Foundation

public enum EnvelopeError: Error { case invalidRequest }

/// Immutable wire codec. The caller owns durable ordering and supplies the operation ID.
/// The same canonical request bytes bind the operation and encrypt its envelope.
public struct EnvelopeCodec: Sendable {
    private let macID: String
    private let deviceID: String
    private let signing: P256.Signing.PrivateKey
    private let agreement: P256.KeyAgreement.PrivateKey
    private let macSigning: P256.Signing.PublicKey
    private let macAgreement: P256.KeyAgreement.PublicKey
    private let now: @Sendable () -> UInt64
    public init(macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
                agreement: P256.KeyAgreement.PrivateKey, macSigning: P256.Signing.PublicKey,
                macAgreement: P256.KeyAgreement.PublicKey,
                now: @escaping @Sendable () -> UInt64) {
        self.macID = macID; self.deviceID = deviceID; self.signing = signing
        self.agreement = agreement; self.macSigning = macSigning; self.macAgreement = macAgreement
        self.now = now
    }
    private func requestBytes(method: String, path: String, body: Data?) throws -> Data {
        var payload: [String: Any] = ["method": method, "path": path]
        if let body { payload["body"] = try JSONSerialization.jsonObject(with: body, options: [.fragmentsAllowed]) }
        return try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    }

    /// Whether an envelope this device sealed can still be sent as is: the relay and the Mac refuse
    /// it once it expires, and it must arrive with time to spare.
    public func resendable(_ envelope: Envelope) -> Bool {
        let (deadline, overflow) = now().addingReportingOverflow(Self.resendMargin)
        return !overflow && deadline < envelope.expiresAtMilliseconds
    }
    static let resendMargin: UInt64 = 15_000

    public func semanticTag(method: String, path: String, body: Data?) throws -> Data {
        let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: agreement.rawRepresentation),
            salt: Data(), info: Data("alondra-operation-binding-v1".utf8), outputByteCount: 32)
        return Data(HMAC<SHA256>.authenticationCode(for: try requestBytes(method: method, path: path, body: body), using: key))
    }

    public func sealRequest(requestID: String, method: String, path: String, body: Data?) throws -> Envelope {
        let plaintext = try requestBytes(method: method, path: path, body: body)
        let timestamp = now()
        guard timestamp <= UInt64.max - 120_000 else { throw EnvelopeError.invalidRequest }
        return try Envelope.seal(route: macID, sender: deviceID,
            recipient: macID, requestID: requestID, kind: "request",
            expiresAtMilliseconds: timestamp + 120_000, plaintext: plaintext,
            senderKey: signing, recipientKey: macAgreement)
    }

    /// The Mac's sealed answer to `requestID`, whichever carrier brought it.
    public func opened(_ envelope: Envelope, requestID: String) throws -> APIResponse {
        try opened(envelope, requestID: requestID, nowMilliseconds: now())
    }

    private func opened(_ envelope: Envelope, requestID: String, nowMilliseconds: UInt64?) throws -> APIResponse {
        guard envelope.kind == "response", envelope.requestID == requestID else {
            throw RelayWireError.invalidResponse
        }
        let bytes = try envelope.open(senderKey: macSigning, recipientKey: agreement,
            sender: macID, recipient: deviceID, route: macID, nowMilliseconds: nowMilliseconds)
        guard let json = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              let status = json["status"] as? Int, (100...599).contains(status) else { throw RelayWireError.invalidResponse }
        if json["error"] as? String == "uncertain" || status >= 500 { return .unknown }
        let body = try JSONSerialization.data(withJSONObject: json["body"] ?? json, options: [.fragmentsAllowed, .sortedKeys])
        return .completed(status: status, body: body)
    }

    func recoveryRequest(_ original: APIRequest, envelope: Envelope, offset: Int? = nil) throws -> APIRequest {
        guard original.method == "POST", original.path == "/actions",
              envelope.requestID == original.operationID, let body = original.body,
              let json = try JSONSerialization.jsonObject(with: body) as? [String: Any],
              let epoch = json["data_epoch"] as? String else { throw OperationError.invalidEndpoint }
        var query = [("digest", try envelope.recoveryDigest()), ("data_epoch", epoch)]
        if let offset { query.append(("offset", String(offset))) }
        return try APIRequest(endpoint: Endpoint<Data>.data(.get,
            path: ["operation-results", original.operationID], query: query), operationID: UUID().uuidString)
    }

    func recover(_ original: APIRequest, envelope: Envelope,
                 read: @Sendable (APIRequest) async throws -> APIResponse) async throws -> APIResponse {
        try await lookup(original, envelope: envelope, read: read).response
    }

    /// `.absent` only when the Mac itself sealed a 404: it holds no finished result for these bytes.
    func lookup(_ original: APIRequest, envelope: Envelope,
                read: @Sendable (APIRequest) async throws -> APIResponse) async throws -> OperationLookup {
        struct Attestation: Decodable {
            let request_id: String
            let digest: String
            let data_epoch: String
            let response: Envelope?
            let offset: Int?
            let total: Int?
            let chunk: Data?
        }
        let first = try recoveryRequest(original, envelope: envelope)
        let epoch = URLComponents(string: first.path)?.queryItems?.first(where: { $0.name == "data_epoch" })?.value
        let digest = try envelope.recoveryDigest()
        var bytes = Data()
        var total: Int?
        for index in 0..<4 {
            try Task.checkCancellation()
            let request = index == 0 ? first : try recoveryRequest(original, envelope: envelope, offset: bytes.count)
            let answer = try await read(request)
            if index == 0, case .completed(404, _) = answer { return .absent }
            guard case let .completed(status, body) = answer, status == 200 else { return .unknown }
            let part = try JSONDecoder().decode(Attestation.self, from: body)
            guard part.request_id == original.operationID, part.digest == digest, part.data_epoch == epoch else {
                throw RelayWireError.invalidResponse
            }
            if let response = part.response {
                guard index == 0, part.chunk == nil, part.offset == nil, part.total == nil else {
                    throw RelayWireError.invalidResponse
                }
                return try Self.lookup(opened(response, requestID: original.operationID, nowMilliseconds: nil))
            }
            guard let chunk = part.chunk, let size = part.total, size > 0, size <= DirectHTTP.maxResponse,
                  total == nil || total == size, part.offset == bytes.count,
                  !chunk.isEmpty, chunk.count <= 256 * 1024, bytes.count + chunk.count <= size,
                  chunk.count == min(256 * 1024, size - bytes.count) else { throw RelayWireError.invalidResponse }
            total = size
            bytes.append(chunk)
            if bytes.count == size {
                let response = try JSONDecoder().decode(Envelope.self, from: bytes)
                // Every part crossed the fresh authenticated carrier gate and binds this digest
                // and epoch. The reconstructed old answer still verifies its pinned signature.
                return try Self.lookup(opened(response, requestID: original.operationID, nowMilliseconds: nil))
            }
        }
        throw RelayWireError.invalidResponse
    }

    private static func lookup(_ response: APIResponse) -> OperationLookup {
        switch response {
        case let .completed(status, body): .completed(status: status, body: body)
        case .unknown: .unknown
        }
    }
}
