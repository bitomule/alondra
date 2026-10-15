import Foundation

/// The Mac's answer to the sealed `GET /routes`: where it can be reached without the relay.
public struct DirectRoutesReply: Decodable, Sendable {
    struct Tailnet: Decodable, Sendable {
        let ip: String
        let name: String?
    }

    let port: UInt16
    let lan: [String]
    let tailscale: Tailnet?
    let iroh: String?

    enum CodingKeys: String, CodingKey {
        case port, lan, tailscale
        case iroh = "iroh_endpoint"
    }

    /// The Mac's iroh endpoint ID, when it has one.
    public var irohEndpoint: String? { iroh.flatMap { IrohIdentity.validEndpointID($0) ? $0 : nil } }

    public var endpoints: [DirectEndpoint] {
        var all = lan.map { DirectEndpoint(host: $0, port: port, kind: .localNetwork) }
        if let tailscale {
            all.append(DirectEndpoint(host: tailscale.ip, port: port, kind: .tailscale))
            if let name = tailscale.name?.trimmingCharacters(in: CharacterSet(charactersIn: ".")), !name.isEmpty {
                all.append(DirectEndpoint(host: name, port: port, kind: .tailscale))
            }
        }
        return all.filter(\.isValid)
    }
}
