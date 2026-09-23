import CryptoKit
import Foundation

public protocol RequestSigner: Sendable {
    var deviceID: String { get }
    func signature(for message: Data) throws -> Data
}

public enum RequestSignature {
    public static let deviceHeader = "X-Alondra-Device"
    public static let timestampHeader = "X-Alondra-Timestamp"
    public static let signatureHeader = "X-Alondra-Signature"

    public static func message(method: String, target: String, timestamp: Int64, body: Data) -> Data {
        let digest = SHA256.hash(data: body).map { String(format: "%02x", $0) }.joined()
        return Data("\(method.uppercased())\n\(target)\n\(timestamp)\n\(digest)".utf8)
    }

    public static func target(of url: URL) -> String {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return url.path() }
        let path = components.percentEncodedPath.isEmpty ? "/" : components.percentEncodedPath
        guard let query = components.percentEncodedQuery else { return path }
        return "\(path)?\(query)"
    }

    public static func sign(_ request: inout URLRequest, with signer: any RequestSigner, now: Date = .now) throws {
        guard let url = request.url else { return }
        let timestamp = Int64((now.timeIntervalSince1970 * 1000).rounded(.down))
        let message = message(method: request.httpMethod ?? "GET", target: target(of: url), timestamp: timestamp, body: request.httpBody ?? Data())
        let signature = try signer.signature(for: message)
        request.setValue(signer.deviceID, forHTTPHeaderField: deviceHeader)
        request.setValue(String(timestamp), forHTTPHeaderField: timestampHeader)
        request.setValue(signature.base64EncodedString(), forHTTPHeaderField: signatureHeader)
    }
}

public enum DeviceSigningKey: Sendable {
    case secureEnclave(SecureEnclave.P256.Signing.PrivateKey)
    case software(P256.Signing.PrivateKey)

    public static func generate() throws -> DeviceSigningKey {
        if SecureEnclave.isAvailable {
            return .secureEnclave(try SecureEnclave.P256.Signing.PrivateKey())
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
