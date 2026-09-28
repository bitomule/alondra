import CryptoKit
import Foundation

/// A Secure Enclave key's `dataRepresentation` is wrapped by this Mac's enclave and useless
/// anywhere else, so it can live in a file. The software key is the fallback for Macs without one.
public enum LoopbackSigningKey: Sendable {
    case secureEnclave(SecureEnclave.P256.Signing.PrivateKey)
    case software(P256.Signing.PrivateKey)

    public static func generate() -> LoopbackSigningKey {
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

    func signature(for message: Data) throws -> Data {
        switch self {
        case .secureEnclave(let key): try key.signature(for: message).derRepresentation
        case .software(let key): try key.signature(for: message).derRepresentation
        }
    }

    var archived: Data {
        switch self {
        case .secureEnclave(let key): Data("E".utf8) + key.dataRepresentation
        case .software(let key): Data("S".utf8) + key.rawRepresentation
        }
    }

    init(archived: Data) throws {
        let payload = archived.dropFirst()
        switch archived.first {
        case UInt8(ascii: "E"): self = .secureEnclave(try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: payload))
        case UInt8(ascii: "S"): self = .software(try P256.Signing.PrivateKey(rawRepresentation: payload))
        default: throw LoopbackSessionError.invalidIdentity
        }
    }
}
