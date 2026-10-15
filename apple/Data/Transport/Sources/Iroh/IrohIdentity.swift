import CryptoKit
import Foundation

/// This device's iroh key for one paired Mac. It is derived from that pairing's signing key, so
/// it lives and dies with the pairing and needs no storage of its own. The relay lets one iroh
/// key speak for one paired device only, which is why each pairing has its own.
public struct IrohIdentity: Sendable, Equatable {
    /// The Ed25519 seed iroh takes as its secret key.
    public let secretKey: Data
    public let endpointID: Data

    public init(pairingSigning: P256.Signing.PrivateKey, macID: String) {
        let derived = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: pairingSigning.rawRepresentation),
            salt: Data("alondra-iroh-endpoint-v1".utf8), info: Data(macID.utf8), outputByteCount: 32)
        self.init(seed: derived.withUnsafeBytes { Data($0) })
    }

    init(seed: Data) {
        secretKey = seed
        // A 32-byte seed is always a valid Ed25519 key.
        endpointID = try! Curve25519.Signing.PrivateKey(rawRepresentation: seed).publicKey.rawRepresentation
    }

    public var endpointIDHex: String { Self.hex(endpointID) }

    /// The body of `PUT /v1/iroh-node`: this endpoint signs the device ID it speaks for.
    func nodeBinding(deviceID: String) throws -> Data {
        let key = try Curve25519.Signing.PrivateKey(rawRepresentation: secretKey)
        let proof = try key.signature(for: Data("alondra-iroh-node-v1\0".utf8) + Data(deviceID.utf8))
        return try JSONSerialization.data(withJSONObject: ["iroh_node_id": endpointIDHex, "proof": Base64URL.encode(proof)],
                                          options: [.sortedKeys])
    }

    public static func validEndpointID(_ value: String) -> Bool {
        value.utf8.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }

    static func endpointBytes(_ value: String) -> Data? {
        guard validEndpointID(value) else { return nil }
        var bytes = Data(capacity: 32)
        var index = value.startIndex
        while index < value.endIndex {
            let next = value.index(index, offsetBy: 2)
            bytes.append(UInt8(value[index..<next], radix: 16)!)
            index = next
        }
        return bytes
    }

    static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}
