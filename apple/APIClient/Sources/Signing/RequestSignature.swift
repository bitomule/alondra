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

    public static func sign(_ request: inout URLRequest, with signer: any RequestSigner, now: Date = .now, body: Data? = nil) throws {
        guard let url = request.url else { return }
        let timestamp = Int64((now.timeIntervalSince1970 * 1000).rounded(.down))
        let message = message(method: request.httpMethod ?? "GET", target: target(of: url), timestamp: timestamp, body: body ?? request.httpBody ?? Data())
        let signature = try signer.signature(for: message)
        request.setValue(signer.deviceID, forHTTPHeaderField: deviceHeader)
        request.setValue(String(timestamp), forHTTPHeaderField: timestampHeader)
        request.setValue(signature.base64EncodedString(), forHTTPHeaderField: signatureHeader)
    }
}
