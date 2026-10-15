import CryptoKit
import Foundation
import AlondraTransport

public enum PairingError: LocalizedError, Equatable, Sendable {
    case invalidInvitation
    case invalidIdentity
    case expired
    case invalidApproval
    case unexpectedStatus(Int)
    case invalidResponse
    case missingAttempt
    case alreadyLinked
    case denied
    case busy
    case macUnreachable
    case requestExpired
    case thisMac
    case insecureAddress
    public var errorDescription: String? {
        switch self {
        case .alreadyLinked: String(localized: "This Mac is already linked. Select it in Settings.")
        case .denied: String(localized: "The Mac didn’t allow this device.")
        case .busy: String(localized: "The Mac has too many requests right now. Try again in a few minutes.")
        case .macUnreachable: String(localized: "Can’t reach that Mac. Check that it’s on and on this network.")
        case .requestExpired: String(localized: "The request expired before the Mac answered. Try again.")
        case .thisMac: String(localized: "That’s this Mac. Pick another one.")
        case .insecureAddress: String(localized: "This device only connects to that address securely. Use the Mac’s IP address, like 192.168.1.20 or 100.101.2.3, or its name ending in .ts.net.")
        case .invalidInvitation: String(localized: "This QR code isn’t from Alondra. Scan the code your Mac shows.")
        case .expired: String(localized: "This QR code has expired. Show a new one on your Mac and scan it.")
        case .invalidApproval, .invalidIdentity, .invalidResponse, .missingAttempt, .unexpectedStatus:
            String(localized: "Linking didn’t finish. Show a new QR code on your Mac and scan it again.")
        }
    }
}

/// Issued by the OIDC login owner. The service never parses an unverified JWT to infer `ownerSub`.
public struct PairingAuthorization: Sendable {
    public let bearerToken: String
    public let ownerSub: String

    public init(bearerToken: String, ownerSub: String) throws {
        guard !bearerToken.isEmpty, !bearerToken.contains(where: \.isNewline),
              !ownerSub.isEmpty, ownerSub.utf8.count <= 256 else {
            throw PairingError.invalidIdentity
        }
        self.bearerToken = bearerToken
        self.ownerSub = ownerSub
    }
}

public struct PairingInvitation: Decodable, Sendable {
    public let version: Int
    public let url: URL
    public let macID: String
    public let macSigningKey: String
    public let macAgreementKey: String
    public let sessionID: String
    public let secret: String
    public let expiresAtMs: UInt64
    /// The Mac's iroh endpoint ID; a Mac without iroh leaves it out.
    public let macIrohEndpoint: String?

    enum CodingKeys: String, CodingKey {
        case version, url, secret
        case macIrohEndpoint = "mac_iroh_endpoint"
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
              (try? P256.KeyAgreement.PublicKey(x963Representation: agreementData)) != nil,
              macIrohEndpoint.map(IrohIdentity.validEndpointID) ?? true else {
            throw PairingError.invalidInvitation
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
/// A pairing made on the local network has no relay and no account owner.
public struct ApprovedServicePairing: Codable, Sendable {
    public let relayURL: URL?
    public let macID: String
    public let deviceID: String
    public let ownerSub: String?
    public let macSigningKey: String
    public let macAgreementKey: String
    public let signingPrivateKey: Data
    public let agreementPrivateKey: Data
    public let approvedAtMs: UInt64
    public var name: String? = nil
    /// Direct addresses learned from Bonjour, the pairing itself and the Mac's `/routes`.
    public var routes: [DirectEndpoint]? = nil
    /// The Mac's iroh endpoint ID, from the pairing or the Mac's `/routes`.
    public var irohEndpoint: String? = nil

    public init(relayURL: URL?, macID: String, deviceID: String, ownerSub: String?, macSigningKey: String,
                macAgreementKey: String, signingPrivateKey: Data, agreementPrivateKey: Data, approvedAtMs: UInt64) {
        self.relayURL = relayURL; self.macID = macID; self.deviceID = deviceID; self.ownerSub = ownerSub
        self.macSigningKey = macSigningKey; self.macAgreementKey = macAgreementKey
        self.signingPrivateKey = signingPrivateKey; self.agreementPrivateKey = agreementPrivateKey
        self.approvedAtMs = approvedAtMs
    }

    public var hasRelay: Bool { relayURL != nil && ownerSub != nil }

    public var pinnedIdentity: DirectMacIdentity {
        DirectMacIdentity(macID: macID, name: name ?? "", signingKey: macSigningKey, agreementKey: macAgreementKey)
    }

    public func macSigningPublicKey() throws -> P256.Signing.PublicKey {
        guard let bytes = PairingWire.decode(macSigningKey) else { throw PairingError.invalidIdentity }
        return try P256.Signing.PublicKey(x963Representation: bytes)
    }

    public func macAgreementPublicKey() throws -> P256.KeyAgreement.PublicKey {
        guard let bytes = PairingWire.decode(macAgreementKey) else { throw PairingError.invalidIdentity }
        return try P256.KeyAgreement.PublicKey(x963Representation: bytes)
    }
}

struct PairingCandidate: Codable {
    let secret: String
    let deviceID: String
    let signingKey: String
    let envelope: Envelope

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
