import Foundation

/// Whether devices away from home reach this Mac directly instead of through the relay, as its
/// daemon diagnoses it on `GET /settings`. Codes a newer daemon adds are read as unknown, never
/// as a failure to load the settings.
public struct DirectConnection: Equatable, Sendable, Decodable {
    public enum Verdict: String, Sendable {
        case reachable, relayOnly = "relay_only", checking, unknown
    }

    public enum Reason: String, Sendable, CaseIterable {
        case localNetworkDenied = "local_network_denied"
        case relayOff = "relay_off"
        case relayDisconnected = "relay_disconnected"
        case captivePortal = "captive_portal"
        case relayOnSameNetwork = "relay_on_same_network"
        case noUdp = "no_udp"
        case symmetricNat = "symmetric_nat"
        case routerOffersNoMapping = "router_offers_no_mapping"
        case noPortMapping = "no_port_mapping"
        case noIpv6 = "no_ipv6"
        case portNotFixed = "port_not_fixed"
    }

    public enum Nat: String, Sendable {
        case none, easy, symmetric, unknown
    }

    public struct PortMapping: Equatable, Sendable, Decodable {
        public let upnp: Bool
        public let natPmp: Bool
        public let pcp: Bool
        public let mapped: String?
        public init(upnp: Bool = false, natPmp: Bool = false, pcp: Bool = false, mapped: String? = nil) {
            self.upnp = upnp; self.natPmp = natPmp; self.pcp = pcp; self.mapped = mapped
        }
    }

    public struct Sessions: Equatable, Sendable, Decodable {
        public let lan: Int
        public let internet: Int
        public let relay: Int
        public init(lan: Int = 0, internet: Int = 0, relay: Int = 0) {
            self.lan = lan; self.internet = internet; self.relay = relay
        }
    }

    public let verdict: Verdict
    public let confirmed: Bool
    public let reasons: [Reason]
    public let portMapping: PortMapping?
    public let publicIpv4: String?
    public let publicIpv6: String?
    public let nat: Nat
    public let port: Int?
    public let portFixed: Bool
    public let lanIp: String?
    public let sessions: Sessions

    public init(verdict: Verdict, confirmed: Bool = false, reasons: [Reason] = [], portMapping: PortMapping? = nil,
                publicIpv4: String? = nil, publicIpv6: String? = nil, nat: Nat = .unknown, port: Int? = nil,
                portFixed: Bool = true, lanIp: String? = nil, sessions: Sessions = Sessions()) {
        self.verdict = verdict; self.confirmed = confirmed; self.reasons = reasons; self.portMapping = portMapping
        self.publicIpv4 = publicIpv4; self.publicIpv6 = publicIpv6; self.nat = nat; self.port = port
        self.portFixed = portFixed; self.lanIp = lanIp; self.sessions = sessions
    }

    private enum CodingKeys: String, CodingKey {
        case verdict, confirmed, reasons, portMapping, publicIpv4, publicIpv6, nat, port, portFixed, lanIp, sessions
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        verdict = Verdict(rawValue: try values.decode(String.self, forKey: .verdict)) ?? .unknown
        confirmed = try values.decodeIfPresent(Bool.self, forKey: .confirmed) ?? false
        reasons = (try values.decodeIfPresent([String].self, forKey: .reasons) ?? []).compactMap(Reason.init(rawValue:))
        portMapping = try values.decodeIfPresent(PortMapping.self, forKey: .portMapping)
        publicIpv4 = try values.decodeIfPresent(String.self, forKey: .publicIpv4)
        publicIpv6 = try values.decodeIfPresent(String.self, forKey: .publicIpv6)
        nat = (try values.decodeIfPresent(String.self, forKey: .nat)).flatMap(Nat.init(rawValue:)) ?? .unknown
        port = try values.decodeIfPresent(Int.self, forKey: .port)
        portFixed = try values.decodeIfPresent(Bool.self, forKey: .portFixed) ?? true
        lanIp = try values.decodeIfPresent(String.self, forKey: .lanIp)
        sessions = try values.decodeIfPresent(Sessions.self, forKey: .sessions) ?? Sessions()
    }
}

extension DirectConnection {
    public var status: LocalizedStringResource {
        switch verdict {
        case .reachable: "Devices can connect directly"
        case .relayOnly: "Away from home, devices go through the relay"
        case .checking: "Checking…"
        case .unknown: "Can’t check right now"
        }
    }

    public var detail: LocalizedStringResource? {
        switch verdict {
        case .reachable where confirmed: "A device away from home already connected directly."
        case .reachable: "Even from a mobile network."
        case .relayOnly: "It works, a bit slower. The steps below can make it direct."
        case .checking, .unknown: nil
        }
    }

    public var portMappingText: LocalizedStringResource? {
        guard let mapping = portMapping else { return nil }
        if let mapped = mapping.mapped { return "Open (\(mapped))" }
        let offered = [mapping.upnp ? "UPnP" : nil, mapping.natPmp ? "NAT-PMP" : nil, mapping.pcp ? "PCP" : nil].compactMap(\.self)
        if offered.isEmpty { return "Not offered by the router" }
        let names = offered.joined(separator: ", ")
        return "Offered (\(names)), no port open"
    }

    public var natText: LocalizedStringResource {
        switch nat {
        case .none: "None, public address"
        case .easy: "Keeps the port"
        case .symmetric: "Changes the port per destination"
        case .unknown: "Unknown"
        }
    }

    public var sessionsText: LocalizedStringResource? {
        let direct = sessions.lan + sessions.internet
        guard direct + sessions.relay > 0 else { return nil }
        return "\(direct) direct · \(sessions.relay) via the relay"
    }

    public static func explanation(_ reason: Reason) -> LocalizedStringResource {
        switch reason {
        case .localNetworkDenied:
            "macOS doesn’t let Alondra use the local network. Allow it in System Settings, Privacy & Security, Local Network."
        case .relayOff: "The relay is off, so this Mac can’t see how it looks from outside."
        case .relayDisconnected: "This Mac isn’t connected to the relay right now."
        case .captivePortal: "This network wants a sign-in page first."
        case .relayOnSameNetwork:
            "The relay is on this same network, so this Mac can’t see its public address. Direct connections from outside work once the relay is outside your home."
        case .noUdp: "This network doesn’t let UDP out, and direct connections need it."
        case .symmetricNat: "The router changes the port for every destination, so devices can’t find a way in."
        case .routerOffersNoMapping: "The router doesn’t open ports by itself (UPnP, NAT-PMP or PCP)."
        case .noPortMapping: "The router can open ports by itself but didn’t open one for this Mac."
        case .noIpv6: "This network has no IPv6."
        case .portNotFixed: "Another app uses the usual port, so this one changes when Alondra restarts and forwarding it won’t last."
        }
    }

    /// The router steps that would give this Mac a way in, with its own port and address.
    public var guide: [LocalizedStringResource] {
        guard verdict == .relayOnly else { return [] }
        var steps: [LocalizedStringResource] = [
            "Open your router’s settings: its app, or its address in a browser.",
            reasons.contains(.noPortMapping)
                ? "The router answers UPnP or NAT-PMP but refused this Mac: check they are fully on and not limited to some devices."
                : "Turn on UPnP or NAT-PMP. Some routers call it PCP or automatic port forwarding.",
        ]
        if let port, let lanIp {
            let port = String(port)
            steps.append("Or forward UDP port \(port) to \(lanIp), this Mac.")
            steps.append("With IPv6, allow incoming UDP to this Mac on port \(port) in the router’s firewall.")
        }
        steps.append("This section updates by itself in a few minutes.")
        return steps
    }
}
