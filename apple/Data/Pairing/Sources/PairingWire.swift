import CryptoKit
import Foundation
import Security

enum PairingWire {
    static func encode(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    static func decode(_ text: String) -> Data? {
        guard !text.contains("="), text.utf8.allSatisfy({
            (48...57).contains($0) || (65...90).contains($0) ||
            (97...122).contains($0) || $0 == 45 || $0 == 95
        }) else { return nil }
        let base = text.replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        guard let data = Data(base64Encoded: base + String(repeating: "=", count: (4 - base.count % 4) % 4)),
              encode(data) == text else { return nil }
        return data
    }

    static func appendFrame(_ bytes: Data, to output: inout Data) {
        var size = UInt32(bytes.count).bigEndian
        withUnsafeBytes(of: &size) { output.append(contentsOf: $0) }
        output.append(bytes)
    }

    static func bigEndian(_ value: UInt64) -> Data {
        var value = value.bigEndian
        return withUnsafeBytes(of: &value) { Data($0) }
    }

    static func random(_ count: Int) throws -> Data {
        var bytes = Data(count: count)
        let status = bytes.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, count, $0.baseAddress!) }
        guard status == errSecSuccess else { throw PairingError.invalidIdentity }
        return bytes
    }
}

extension PairingApproval {
    func signedBytes() throws -> Data {
        guard v == 1 else { throw PairingError.invalidApproval }
        var bytes = Data("alondra-service-pairing-approval-v1\0".utf8)
        bytes.append(1)
        for value in [sessionID, ownerSub, macID, macSigningKey, macAgreementKey,
                      deviceID, phoneSigningKey, phoneAgreementKey] {
            PairingWire.appendFrame(Data(value.utf8), to: &bytes)
        }
        bytes.append(PairingWire.bigEndian(expiresAtMs))
        return bytes
    }
}

extension PairingApprovalProof {
    func verify(
        invitation: PairingInvitation, authorization: PairingAuthorization,
        deviceID: String, signingKey: String, agreementKey: String, nowMs: UInt64
    ) throws {
        let a = approval
        guard a.v == 1, a.sessionID == invitation.sessionID,
              a.ownerSub == authorization.ownerSub, a.macID == invitation.macID,
              a.macSigningKey == invitation.macSigningKey,
              a.macAgreementKey == invitation.macAgreementKey,
              a.deviceID == deviceID, a.phoneSigningKey == signingKey,
              a.phoneAgreementKey == agreementKey,
              a.expiresAtMs == invitation.expiresAtMs, nowMs < a.expiresAtMs,
              let publicBytes = PairingWire.decode(invitation.macSigningKey),
              let signatureBytes = PairingWire.decode(signature), signatureBytes.count == 64,
              let publicKey = try? P256.Signing.PublicKey(x963Representation: publicBytes),
              let signature = try? P256.Signing.ECDSASignature(rawRepresentation: signatureBytes),
              publicKey.isValidSignature(signature, for: try a.signedBytes()) else {
            throw PairingError.invalidApproval
        }
    }
}
