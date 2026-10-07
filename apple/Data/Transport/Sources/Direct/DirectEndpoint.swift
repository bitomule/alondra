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

    /// "host" or "host:port" as the person typed it; the direct port is 47810 unless they say
    /// otherwise. A tailnet IP or a `.ts.net` name is a Tailscale address.
    public init?(typed text: String) {
        var host = text.trimmingCharacters(in: .whitespaces)
        for scheme in ["http://", "https://"] where host.lowercased().hasPrefix(scheme) { host.removeFirst(scheme.count) }
        while host.hasSuffix("/") { host.removeLast() }
        var port: UInt16 = 47810
        if host.hasPrefix("["), let close = host.firstIndex(of: "]") {
            let rest = host[host.index(after: close)...]
            if rest.hasPrefix(":") {
                guard let parsed = UInt16(rest.dropFirst()) else { return nil }
                port = parsed
            } else if !rest.isEmpty { return nil }
            host = String(host[host.index(after: host.startIndex)..<close])
        } else if host.filter({ $0 == ":" }).count == 1, let colon = host.lastIndex(of: ":") {
            guard let parsed = UInt16(host[host.index(after: colon)...]) else { return nil }
            port = parsed
            host = String(host[..<colon])
        }
        let octets = host.split(separator: ".").compactMap { UInt8($0) }
        let tailnet = octets.count == 4 && octets[0] == 100 && (64..<128).contains(octets[1])
            || host.lowercased().hasSuffix(".ts.net")
        self.init(host: host, port: port, kind: tailnet ? .tailscale : .localNetwork)
        guard isValid else { return nil }
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
