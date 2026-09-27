import CryptoKit
import Foundation

public enum ServiceEnvelopeError: Error { case invalidRequest }

/// Immutable wire codec. The caller owns durable ordering and supplies the operation ID.
/// The same canonical request bytes bind the operation and encrypt its envelope.
public struct ServiceEnvelopeCodec: Sendable {
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

    public func semanticTag(method: String, path: String, body: Data?) throws -> Data {
        let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: agreement.rawRepresentation),
            salt: Data(), info: Data("alondra-operation-binding-v1".utf8), outputByteCount: 32)
        return Data(HMAC<SHA256>.authenticationCode(for: try requestBytes(method: method, path: path, body: body), using: key))
    }

    public func sealRequest(requestID: String, method: String, path: String, body: Data?) throws -> EnvelopeV1 {
        let plaintext = try requestBytes(method: method, path: path, body: body)
        let timestamp = now()
        guard timestamp <= UInt64.max - 120_000 else { throw ServiceEnvelopeError.invalidRequest }
        return try EnvelopeV1.seal(route: macID, sender: deviceID,
            recipient: macID, requestID: requestID, kind: "request",
            expiresAtMilliseconds: timestamp + 120_000, plaintext: plaintext,
            senderKey: signing, recipientKey: macAgreement)
    }

    public func verified(_ receipt: RelayReceipt, requestID: String, allowOverviewTooLarge: Bool = false) throws -> ServiceResponse? {
        guard receipt.requestID == requestID else { throw RelayWireError.invalidResponse }
        guard receipt.status == "completed" else { return nil }
        guard let envelope = receipt.response, envelope.kind == "response", envelope.requestID == requestID else {
            throw RelayWireError.invalidResponse
        }
        let bytes = try envelope.open(senderKey: macSigning, recipientKey: agreement,
            sender: macID, recipient: deviceID, route: macID, nowMilliseconds: now())
        guard let json = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              let status = json["status"] as? Int, (100...599).contains(status) else { throw RelayWireError.invalidResponse }
        // This opt-in is used only by the overview reader after signature/decryption checks.
        if allowOverviewTooLarge, status == 502, json["error"] as? String == "response_too_large" {
            return .completed(status: status, body: Data(#"{"error":"response_too_large"}"#.utf8))
        }
        if json["error"] as? String == "uncertain" || status >= 500 { return .unknown }
        let body = try JSONSerialization.data(withJSONObject: json["body"] ?? json, options: [.fragmentsAllowed, .sortedKeys])
        return .completed(status: status, body: body)
    }

}
