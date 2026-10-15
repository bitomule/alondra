import CryptoKit
import Foundation

/// The first stream of every connection: the paired device proves itself, bound to both
/// endpoints so the proof works for this connection only. Framed as alondra_e2e::iroh_hello_bytes.
enum IrohHello {
    static func bytes(macID: String, deviceID: String, deviceEndpoint: Data, macEndpoint: Data) throws -> Data {
        guard RelayHTTPChallenge.validText(macID), RelayHTTPChallenge.validText(deviceID),
              deviceEndpoint.count == 32, macEndpoint.count == 32 else { throw RelayWireError.invalidField }
        var bytes = Data("alondra-iroh-hello-v1".utf8)
        try WireFrame.append(macID, to: &bytes)
        try WireFrame.append(deviceID, to: &bytes)
        try WireFrame.append(deviceEndpoint, to: &bytes)
        try WireFrame.append(macEndpoint, to: &bytes)
        return bytes
    }

    static func message(macID: String, deviceID: String, deviceEndpoint: Data, macEndpoint: Data,
                        signing: P256.Signing.PrivateKey) throws -> Data {
        let signature = try signing.signature(for: bytes(macID: macID, deviceID: deviceID,
            deviceEndpoint: deviceEndpoint, macEndpoint: macEndpoint))
        return try JSONSerialization.data(withJSONObject: ["device_id": deviceID,
            "signature": Base64URL.encode(signature.rawRepresentation)], options: [.sortedKeys])
    }

    static func accepted(_ reply: Data) -> Bool {
        (try? JSONSerialization.jsonObject(with: reply) as? [String: Any])?["ok"] as? Bool == true
    }
}
