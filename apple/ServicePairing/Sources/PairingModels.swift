import CryptoKit
import Foundation
import AlondraAPIClient

public enum ServicePairingError: LocalizedError, Equatable, Sendable {
    case invalidInvitation
    case invalidIdentity
    case expired
    case invalidApproval
    case unexpectedStatus(Int)
    case invalidResponse
    case missingAttempt
    case alreadyLinked
    public var errorDescription: String? {
        if case .alreadyLinked = self { return String(localized: "This Mac is already linked. Select it in Settings.") }
        return nil
    }
}

/// Issued by the OIDC login owner. The service never parses an unverified JWT to infer `ownerSub`.
public struct PairingAuthorization: Sendable {
    public let bearerToken: String
    public let ownerSub: String

    public init(bearerToken: String, ownerSub: String) throws {
        guard !bearerToken.isEmpty, !bearerToken.contains(where: \.isNewline),
              !ownerSub.isEmpty, ownerSub.utf8.count <= 256 else {
            throw ServicePairingError.invalidIdentity
        }
        self.bearerToken = bearerToken
        self.ownerSub = ownerSub
    }
}

public struct ServicePairingInvitation: Decodable, Sendable {
    public let version: Int
    public let url: URL
    public let macID: String
    public let macSigningKey: String
    public let macAgreementKey: String
    public let sessionID: String
    public let secret: String
    public let expiresAtMs: UInt64

    enum CodingKeys: String, CodingKey {
        case version, url, secret
        case macID = "mac_id"
        case macSigningKey = "mac_signing_key"
        case macAgreementKey = "mac_agreement_key"
        case sessionID = "session_id"
        case expiresAtMs = "expires_at_ms"
    }

    public init(qrPayload: String) throws {
        self = try JSONDecoder().decode(Self.self, from: Data(qrPayload.utf8))
        guard version == 1, url.scheme == "https", url.host() != nil,
              url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
              url.path.isEmpty || url.path == "/",
              macID.hasPrefix("mac-"), macID.utf8.count <= 128,
              macID.utf8.allSatisfy({ $0.isASCIIAlphaNumericOrID }),
              sessionID.hasPrefix("pair-"),
              sessionID.utf8.count <= 128, sessionID.utf8.allSatisfy({ $0.isASCIIAlphaNumericOrID }),
              let secretData = PairingWire.decode(secret), secretData.count == 16,
              let signingData = PairingWire.decode(macSigningKey), signingData.count == 65,
              let agreementData = PairingWire.decode(macAgreementKey), agreementData.count == 65,
              (try? P256.Signing.PublicKey(x963Representation: signingData)) != nil,
              (try? P256.KeyAgreement.PublicKey(x963Representation: agreementData)) != nil else {
            throw ServicePairingError.invalidInvitation
        }
    }
}

extension UInt8 {
    fileprivate var isASCIIAlphaNumericOrID: Bool {
        (48...57).contains(self) || (65...90).contains(self) ||
        (97...122).contains(self) || self == 45 || self == 95
    }
}

public struct PairingPending: Sendable {
    public let attemptID: UUID
    public let comparisonCode: String
    public let expiresAtMs: UInt64
}

/// One verified, approved pairing. The private keys are stored in this-device-only Keychain data.
public struct ApprovedServicePairing: Codable, Sendable {
    public let relayURL: URL
    public let macID: String
    public let deviceID: String
    public let ownerSub: String
    public let macSigningKey: String
    public let macAgreementKey: String
    public let signingPrivateKey: Data
    public let agreementPrivateKey: Data
    public let approvedAtMs: UInt64

    public func macSigningPublicKey() throws -> P256.Signing.PublicKey {
        guard let bytes = PairingWire.decode(macSigningKey) else { throw ServicePairingError.invalidIdentity }
        return try P256.Signing.PublicKey(x963Representation: bytes)
    }

    public func macAgreementPublicKey() throws -> P256.KeyAgreement.PublicKey {
        guard let bytes = PairingWire.decode(macAgreementKey) else { throw ServicePairingError.invalidIdentity }
        return try P256.KeyAgreement.PublicKey(x963Representation: bytes)
    }
}

struct PairingCandidate: Codable {
    let secret: String
    let deviceID: String
    let signingKey: String
    let envelope: EnvelopeV1

    enum CodingKeys: String, CodingKey {
        case secret, envelope
        case deviceID = "device_id"
        case signingKey = "signing_key"
    }
}

struct PairingCandidatePlain: Encodable {
    let deviceID: String
    let signingKey: String
    let agreementKey: String
    let displayName: String
    let ownerSub: String
    let sessionID: String

    enum CodingKeys: String, CodingKey {
        case deviceID = "device_id"
        case signingKey = "signing_key"
        case agreementKey = "agreement_key"
        case displayName = "display_name"
        case ownerSub = "owner_sub"
        case sessionID = "session_id"
    }
}

struct PairingApprovalProof: Decodable {
    let approval: PairingApproval
    let signature: String
}

struct PairingApproval: Decodable {
    let v: Int
    let sessionID: String
    let ownerSub: String
    let macID: String
    let macSigningKey: String
    let macAgreementKey: String
    let deviceID: String
    let phoneSigningKey: String
    let phoneAgreementKey: String
    let expiresAtMs: UInt64

    enum CodingKeys: String, CodingKey {
        case v
        case sessionID = "session_id"
        case ownerSub = "owner_sub"
        case macID = "mac_id"
        case macSigningKey = "mac_signing_key"
        case macAgreementKey = "mac_agreement_key"
        case deviceID = "device_id"
        case phoneSigningKey = "phone_signing_key"
        case phoneAgreementKey = "phone_agreement_key"
        case expiresAtMs = "expires_at_ms"
    }
}
