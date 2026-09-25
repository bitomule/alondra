import AlondraAPIClient
import CryptoKit
import Foundation

/// The persisted signing key remains device-bound. Its representations are kept compatible
/// with the original AlondraShared implementation; there is no key migration on upgrade.
public enum DeviceSigningKey: Sendable {
    case secureEnclave(SecureEnclave.P256.Signing.PrivateKey)
    case software(P256.Signing.PrivateKey)

    public static func generate() -> DeviceSigningKey {
        if SecureEnclave.isAvailable, let key = try? SecureEnclave.P256.Signing.PrivateKey() {
            return .secureEnclave(key)
        }
        return .software(P256.Signing.PrivateKey())
    }

    public var publicKey: P256.Signing.PublicKey {
        switch self {
        case .secureEnclave(let key): key.publicKey
        case .software(let key): key.publicKey
        }
    }

    public func signature(for message: Data) throws -> P256.Signing.ECDSASignature {
        switch self {
        case .secureEnclave(let key): try key.signature(for: message)
        case .software(let key): try key.signature(for: message)
        }
    }
}

public struct DeviceSigner: RequestSigner {
    public let deviceID: String
    public let key: DeviceSigningKey

    public init(deviceID: String, key: DeviceSigningKey) {
        self.deviceID = deviceID
        self.key = key
    }

    public func signature(for message: Data) throws -> Data {
        try key.signature(for: message).derRepresentation
    }
}
