import CryptoKit
import Foundation

/// What a Mac's `GET /direct/hello` says about itself. Public information: it proves nothing
/// until it matches the keys pinned at pairing.
public struct DirectMacIdentity: Codable, Hashable, Sendable {
    public let macID: String
    public let name: String
    public let signingKey: String
    public let agreementKey: String

    enum CodingKeys: String, CodingKey {
        case name
        case macID = "mac_id"
        case signingKey = "signing_key"
        case agreementKey = "agreement_key"
    }

    public init(macID: String, name: String, signingKey: String, agreementKey: String) {
        self.macID = macID
        self.name = name
        self.signingKey = signingKey
        self.agreementKey = agreementKey
    }

    public func signingPublicKey() throws -> P256.Signing.PublicKey {
        try P256.Signing.PublicKey(x963Representation: Base64URL.decode(signingKey))
    }

    public func agreementPublicKey() throws -> P256.KeyAgreement.PublicKey {
        try P256.KeyAgreement.PublicKey(x963Representation: Base64URL.decode(agreementKey))
    }

    func sameKeys(as other: DirectMacIdentity) -> Bool {
        signingKey == other.signingKey && agreementKey == other.agreementKey
    }

    var isValid: Bool {
        macID.hasPrefix("mac-") && RelayHTTPChallenge.validText(macID) && name.utf8.count <= 256
            && (try? signingPublicKey()) != nil && (try? agreementPublicKey()) != nil
    }
}
