import CryptoKit
import Foundation
import Security

public enum RelayWireError: Error, Equatable, Sendable {
    case invalidEndpoint, invalidField, invalidEncoding, invalidSignature, wrongIdentity, expired
    case invalidResponse, unauthorized, conflict, outcomeUnknown, server(Int)
}

enum Base64URL {
    static func encode(_ bytes: Data) -> String {
        bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }

    static func decode(_ value: String) throws -> Data {
        guard !value.contains("="), value.utf8.allSatisfy({
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
        }) else { throw RelayWireError.invalidEncoding }
        let standard = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        guard let data = Data(base64Encoded: standard + String(repeating: "=", count: (4 - standard.count % 4) % 4)),
              encode(data) == value else { throw RelayWireError.invalidEncoding }
        return data
    }
}

enum WireFrame {
    static func append(_ value: Data, to bytes: inout Data) throws {
        guard value.count <= Int(UInt32.max) else { throw RelayWireError.invalidField }
        var length = UInt32(value.count).bigEndian
        withUnsafeBytes(of: &length) { bytes.append(contentsOf: $0) }
        bytes.append(value)
    }

    static func append(_ value: String, to bytes: inout Data) throws {
        try append(Data(value.utf8), to: &bytes)
    }
}

/// The signed bytes are framed exactly as alondra-e2e::http_challenge_bytes.
public enum RelayHTTPChallenge {
    public static func message(deviceID: String, method: String, target: String,
                               epoch: Data, body: Data, timestampMilliseconds: UInt64,
                               nonce: Data) throws -> Data {
        guard validText(deviceID), !method.isEmpty, method.utf8.count <= 16,
              method.utf8.allSatisfy({ (65...90).contains($0) }),
              target.hasPrefix("/"), target.utf8.count <= 2048,
              epoch.count == 16, nonce.count == 16 else { throw RelayWireError.invalidField }
        var bytes = Data("alondra-http-auth-v1".utf8)
        try WireFrame.append(deviceID, to: &bytes)
        try WireFrame.append(method, to: &bytes)
        try WireFrame.append(target, to: &bytes)
        try WireFrame.append(epoch, to: &bytes)
        try WireFrame.append(Data(SHA256.hash(data: body)), to: &bytes)
        try WireFrame.append(String(timestampMilliseconds), to: &bytes)
        try WireFrame.append(nonce, to: &bytes)
        return bytes
    }

    static func validText(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 256 && value.utf8.allSatisfy { (33...126).contains($0) }
    }
}

public struct EnvelopeV1: Codable, Sendable, Equatable {
    public let v: UInt8
    public let route: String
    public let sender: String
    public let recipient: String
    public let requestID: String
    public let kind: String
    public let expiresAtMilliseconds: UInt64
    public let ephemeralKey: String
    public let nonce: String
    public let ciphertext: String
    public let signature: String

    enum CodingKeys: String, CodingKey {
        case v, route, sender, recipient, kind, nonce, ciphertext, signature
        case requestID = "request_id", expiresAtMilliseconds = "expires_at_ms", ephemeralKey = "ephemeral_key"
    }

    public init(v: UInt8 = 1, route: String, sender: String, recipient: String, requestID: String,
                kind: String, expiresAtMilliseconds: UInt64, ephemeralKey: String, nonce: String,
                ciphertext: String, signature: String) {
        self.v = v; self.route = route; self.sender = sender; self.recipient = recipient
        self.requestID = requestID; self.kind = kind; self.expiresAtMilliseconds = expiresAtMilliseconds
        self.ephemeralKey = ephemeralKey; self.nonce = nonce; self.ciphertext = ciphertext; self.signature = signature
    }

    public func authenticatedHeader() throws -> Data {
        guard v == 1, ["request", "response", "push", "pairing"].contains(kind), expiresAtMilliseconds > 0,
              [route, sender, recipient].allSatisfy(RelayHTTPChallenge.validText),
              !requestID.isEmpty, requestID.utf8.count <= 128,
              requestID.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 })
        else { throw RelayWireError.invalidField }
        let epk = try Base64URL.decode(ephemeralKey)
        let nonceData = try Base64URL.decode(nonce)
        guard epk.count == 65, epk.first == 4, nonceData.count == 12 else { throw RelayWireError.invalidEncoding }
        var bytes = Data("alondra-e2e-envelope-v1".utf8)
        try WireFrame.append(Data([v]), to: &bytes)
        for value in [route, sender, recipient, requestID, kind] { try WireFrame.append(value, to: &bytes) }
        var expiry = expiresAtMilliseconds.bigEndian
        try withUnsafeBytes(of: &expiry) { try WireFrame.append(Data($0), to: &bytes) }
        try WireFrame.append(epk, to: &bytes)
        try WireFrame.append(nonceData, to: &bytes)
        return bytes
    }

    public func signedBytes() throws -> Data {
        var bytes = try authenticatedHeader()
        let cipher = try Base64URL.decode(ciphertext)
        guard cipher.count >= 16, cipher.count <= 20 * 1024 * 1024 + 16 else { throw RelayWireError.invalidField }
        try WireFrame.append(cipher, to: &bytes)
        return bytes
    }

    public func verify(senderKey: P256.Signing.PublicKey, sender expectedSender: String,
                       recipient expectedRecipient: String, route expectedRoute: String,
                       nowMilliseconds: UInt64) throws {
        guard sender == expectedSender, recipient == expectedRecipient, route == expectedRoute else { throw RelayWireError.wrongIdentity }
        guard nowMilliseconds <= expiresAtMilliseconds else { throw RelayWireError.expired }
        let signatureBytes = try Base64URL.decode(signature)
        guard signatureBytes.count == 64,
              let sig = try? P256.Signing.ECDSASignature(rawRepresentation: signatureBytes),
              senderKey.isValidSignature(sig, for: try signedBytes()) else { throw RelayWireError.invalidSignature }
    }

    public func open(senderKey: P256.Signing.PublicKey, recipientKey: P256.KeyAgreement.PrivateKey,
                     sender expectedSender: String, recipient expectedRecipient: String,
                     route expectedRoute: String, nowMilliseconds: UInt64) throws -> Data {
        try verify(senderKey: senderKey, sender: expectedSender, recipient: expectedRecipient,
                   route: expectedRoute, nowMilliseconds: nowMilliseconds)
        let header = try authenticatedHeader()
        let ephemeral = try P256.KeyAgreement.PublicKey(x963Representation: Base64URL.decode(ephemeralKey))
        let shared = try recipientKey.sharedSecretFromKeyAgreement(with: ephemeral)
        let key = shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: Data(SHA256.hash(data: header)),
                                                 sharedInfo: Data("alondra-e2e-key-v1".utf8), outputByteCount: 32)
        let box = try ChaChaPoly.SealedBox(combined: Base64URL.decode(nonce) + Base64URL.decode(ciphertext))
        return try ChaChaPoly.open(box, using: key, authenticating: header)
    }

    public static func seal(route: String, sender: String, recipient: String, requestID: String,
                            kind: String, expiresAtMilliseconds: UInt64, plaintext: Data,
                            senderKey: P256.Signing.PrivateKey, recipientKey: P256.KeyAgreement.PublicKey) throws -> Self {
        guard plaintext.count <= 20 * 1024 * 1024 else { throw RelayWireError.invalidField }
        let ephemeral = P256.KeyAgreement.PrivateKey()
        var random = [UInt8](repeating: 0, count: 12)
        let result = SecRandomCopyBytes(kSecRandomDefault, random.count, &random)
        guard result == errSecSuccess else { throw RelayWireError.invalidEncoding }
        let nonceData = Data(random)
        var envelope = Self(route: route, sender: sender, recipient: recipient, requestID: requestID,
                            kind: kind, expiresAtMilliseconds: expiresAtMilliseconds,
                            ephemeralKey: Base64URL.encode(ephemeral.publicKey.x963Representation),
                            nonce: Base64URL.encode(nonceData), ciphertext: "", signature: "")
        let header = try envelope.authenticatedHeader()
        let shared = try ephemeral.sharedSecretFromKeyAgreement(with: recipientKey)
        let key = shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: Data(SHA256.hash(data: header)),
                                                 sharedInfo: Data("alondra-e2e-key-v1".utf8), outputByteCount: 32)
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: ChaChaPoly.Nonce(data: nonceData), authenticating: header)
        envelope = Self(route: route, sender: sender, recipient: recipient, requestID: requestID,
                        kind: kind, expiresAtMilliseconds: expiresAtMilliseconds,
                        ephemeralKey: envelope.ephemeralKey, nonce: envelope.nonce,
                        ciphertext: Base64URL.encode(box.ciphertext + box.tag), signature: "")
        let signed = try envelope.signedBytes()
        return Self(route: route, sender: sender, recipient: recipient, requestID: requestID,
                    kind: kind, expiresAtMilliseconds: expiresAtMilliseconds,
                    ephemeralKey: envelope.ephemeralKey, nonce: envelope.nonce, ciphertext: envelope.ciphertext,
                    signature: Base64URL.encode(try senderKey.signature(for: signed).rawRepresentation))
    }
}
