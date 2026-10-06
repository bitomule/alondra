import AlondraContract
import Foundation

/// One address a Mac answers on directly, and which network reaches it.
public struct DirectEndpoint: Codable, Hashable, Sendable {
    public enum Kind: String, Codable, Sendable { case localNetwork, tailscale }

    public let host: String
    public let port: UInt16
    public let kind: Kind

    public init(host: String, port: UInt16, kind: Kind) {
        self.host = host
        self.port = port
        self.kind = kind
    }

    public var route: ConnectionRoute { kind == .localNetwork ? .localNetwork : .tailscale }

    /// LAN first, then the tailnet IP, then the tailnet name.
    var rank: Int {
        switch kind {
        case .localNetwork: 0
        case .tailscale: isIPLiteral ? 1 : 2
        }
    }

    var isIPLiteral: Bool {
        host.contains(":") || host.split(separator: ".").count == 4 && host.split(separator: ".").allSatisfy { UInt8($0) != nil }
    }

    func url(_ path: String) -> URL? {
        var components = URLComponents()
        components.scheme = "http"
        components.host = host.contains(":") ? "[\(host)]" : host
        components.port = Int(port)
        components.percentEncodedPath = path
        return components.url
    }

    var isValid: Bool {
        port > 0 && !host.isEmpty && host.utf8.count <= 253 && !host.contains("%") && !host.contains("/")
            && !host.contains(where: \.isWhitespace)
    }
}
