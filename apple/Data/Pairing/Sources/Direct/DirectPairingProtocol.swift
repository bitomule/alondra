import CryptoKit
import Foundation

/// The commit, reveal and compare scheme of `alondra_e2e::pairing`, byte for byte: both
/// screens show the same six characters only when nobody sits between the device and the Mac.
public enum DirectPairingProtocol {
    static let nonceLength = 32
    private static let commitDomain = Data("alondra-direct-pairing-commit\0".utf8)
    private static let codeDomain = Data("alondra-direct-pairing-code\0".utf8)
    private static let transcriptDomain = Data("alondra-direct-pairing-transcript\0".utf8)
    private static let approvalDomain = Data("alondra-direct-pairing-approval\0".utf8)

    static func nonce(_ value: String) throws -> Data {
        guard let bytes = PairingWire.decode(value), bytes.count == nonceLength else { throw PairingError.invalidResponse }
        return bytes
    }

    public static func commitment(clientNonce: String, deviceID: String, signingKey: String,
                                  agreementKey: String) throws -> String {
        var bytes = commitDomain
        PairingWire.appendFrame(try nonce(clientNonce), to: &bytes)
        for value in [deviceID, signingKey, agreementKey] { PairingWire.appendFrame(Data(value.utf8), to: &bytes) }
        return PairingWire.encode(Data(SHA256.hash(data: bytes)))
    }

    /// Everything both sides saw during one attempt. Keys are the base64url wire strings.
    public struct Transcript: Sendable, Equatable {
        public let pairingID: String
        public let macID: String
        public let macSigningKey: String
        public let macAgreementKey: String
        public let deviceID: String
        public let clientSigningKey: String
        public let clientAgreementKey: String
        public let clientNonce: String
        public let macNonce: String

        public init(pairingID: String, macID: String, macSigningKey: String, macAgreementKey: String,
                    deviceID: String, clientSigningKey: String, clientAgreementKey: String,
                    clientNonce: String, macNonce: String) {
            self.pairingID = pairingID; self.macID = macID
            self.macSigningKey = macSigningKey; self.macAgreementKey = macAgreementKey
            self.deviceID = deviceID; self.clientSigningKey = clientSigningKey
            self.clientAgreementKey = clientAgreementKey
            self.clientNonce = clientNonce; self.macNonce = macNonce
        }

        private func bytes(_ domain: Data) throws -> Data {
            var bytes = domain
            for value in [pairingID, macID, macSigningKey, macAgreementKey, deviceID, clientSigningKey, clientAgreementKey] {
                PairingWire.appendFrame(Data(value.utf8), to: &bytes)
            }
            PairingWire.appendFrame(try nonce(clientNonce), to: &bytes)
            PairingWire.appendFrame(try nonce(macNonce), to: &bytes)
            return bytes
        }

        /// Six uppercase hex characters.
        public func code() throws -> String {
            SHA256.hash(data: try bytes(codeDomain)).prefix(3).map { String(format: "%02X", $0) }.joined()
        }

        public func digest() throws -> String {
            PairingWire.encode(Data(SHA256.hash(data: try bytes(transcriptDomain))))
        }
    }

    /// The Mac's signed approval of exactly one transcript.
    public struct Approval: Codable, Sendable, Equatable {
        public let pairingID: String
        public let macID: String
        public let macSigningKey: String
        public let macAgreementKey: String
        public let deviceID: String
        public let clientSigningKey: String
        public let clientAgreementKey: String
        public let transcript: String
        public let expiresAtMs: UInt64

        enum CodingKeys: String, CodingKey {
            case transcript
            case pairingID = "pairing_id"
            case macID = "mac_id"
            case macSigningKey = "mac_signing_key"
            case macAgreementKey = "mac_agreement_key"
            case deviceID = "device_id"
            case clientSigningKey = "client_signing_key"
            case clientAgreementKey = "client_agreement_key"
            case expiresAtMs = "expires_at_ms"
        }

        func signedBytes() -> Data {
            var bytes = approvalDomain
            for value in [pairingID, macID, macSigningKey, macAgreementKey, deviceID,
                          clientSigningKey, clientAgreementKey, transcript] {
                PairingWire.appendFrame(Data(value.utf8), to: &bytes)
            }
            bytes.append(PairingWire.bigEndian(expiresAtMs))
            return bytes
        }

        /// Checks the Mac's signature and that it names exactly the transcript this device ran,
        /// with the keys `hello` returned and the ones this device generated.
        public func verify(signature: String, macKey: P256.Signing.PublicKey, expected: Transcript, nowMs: UInt64) throws {
            guard let raw = PairingWire.decode(signature), raw.count == 64,
                  let parsed = try? P256.Signing.ECDSASignature(rawRepresentation: raw),
                  macKey.isValidSignature(parsed, for: signedBytes()) else { throw PairingError.invalidApproval }
            guard pairingID == expected.pairingID, macID == expected.macID,
                  macSigningKey == expected.macSigningKey, macAgreementKey == expected.macAgreementKey,
                  deviceID == expected.deviceID, clientSigningKey == expected.clientSigningKey,
                  clientAgreementKey == expected.clientAgreementKey,
                  transcript == (try expected.digest()) else { throw PairingError.invalidApproval }
            guard nowMs < expiresAtMs else { throw PairingError.expired }
        }
    }
}
