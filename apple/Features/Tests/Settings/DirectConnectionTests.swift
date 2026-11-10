import AlondraFeatures
import XCTest

final class DirectConnectionTests: XCTestCase {
    private func decoded(_ json: String) throws -> MacSettings {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(MacSettings.self, from: Data(json.utf8))
    }

    private let settings = #"{"projects_root":"/Users/d/Projects","candidates":[],"relay":{"on":true},"error":null"#

    func testReadsTheDiagnosisInTheShapeTheDaemonSends() throws {
        let read = try decoded(settings + #"""
        ,"direct_connection":{"verdict":"reachable","confirmed":false,"reasons":[],
         "port_mapping":{"upnp":true,"nat_pmp":false,"pcp":false,"mapped":"203.0.113.7:61022"},
         "public_ipv4":"203.0.113.7","public_ipv6":null,"nat":"easy","port":47811,"port_fixed":true,
         "lan_ip":"192.168.1.20","sessions":{"lan":0,"internet":0,"relay":0}}}
        """#)
        XCTAssertEqual(read.directConnection, DirectConnection(
            verdict: .reachable, portMapping: .init(upnp: true, mapped: "203.0.113.7:61022"), publicIpv4: "203.0.113.7",
            nat: .easy, port: 47811, lanIp: "192.168.1.20"))
    }

    func testADaemonWithoutTheDiagnosisShowsNoSection() throws {
        XCTAssertNil(try decoded(settings + "}").directConnection)
        XCTAssertNil(try decoded(settings + #","direct_connection":null}"#).directConnection)
    }

    func testCodesFromANewerDaemonNeverBreakTheSettings() throws {
        let read = try decoded(settings + #","direct_connection":{"verdict":"teleport","reasons":["no_ipv6","moon_phase"],"nat":"weird"}}"#)
        XCTAssertEqual(read.directConnection?.verdict, .unknown)
        XCTAssertEqual(read.directConnection?.reasons, [.noIpv6])
        XCTAssertEqual(read.directConnection?.nat, .unknown)
    }

    func testEveryReasonTheDaemonSendsIsKnownHere() {
        let daemon = ["local_network_denied", "relay_off", "relay_disconnected", "captive_portal", "relay_on_same_network",
                      "no_udp", "symmetric_nat", "router_offers_no_mapping", "no_port_mapping", "no_ipv6", "port_not_fixed"]
        XCTAssertEqual(DirectConnection.Reason.allCases.map(\.rawValue), daemon)
        for reason in DirectConnection.Reason.allCases {
            XCTAssertFalse(String(localized: DirectConnection.explanation(reason)).isEmpty)
        }
    }

    func testTheGuideNamesThisMacsPortAndAddressOnlyWhenItGoesThroughTheRelay() {
        let relayOnly = DirectConnection(verdict: .relayOnly, reasons: [.routerOffersNoMapping, .noIpv6],
                                         portMapping: .init(), port: 47811, lanIp: "192.168.1.20")
        XCTAssertEqual(relayOnly.guide.map { String(localized: $0) }, [
            "Open your router’s settings: its app, or its address in a browser.",
            "Turn on UPnP or NAT-PMP. Some routers call it PCP or automatic port forwarding.",
            "Or forward UDP port 47811 to 192.168.1.20, this Mac.",
            "With IPv6, allow incoming UDP to this Mac on port 47811 in the router’s firewall.",
            "This section updates by itself in a few minutes.",
        ])
        XCTAssertEqual(relayOnly.portMappingText.map { String(localized: $0) }, "Not offered by the router")
        let noAddress = DirectConnection(verdict: .relayOnly, port: 47811)
        XCTAssertFalse(noAddress.guide.contains { String(localized: $0).contains("forward UDP") }, "no forwarding step without the Mac's address")
        XCTAssertTrue(DirectConnection(verdict: .reachable, port: 47811, lanIp: "192.168.1.20").guide.isEmpty)
        XCTAssertTrue(DirectConnection(verdict: .checking).guide.isEmpty)
    }

    func testADeniedLocalNetworkAsksForThePermissionInsteadOfTheRouter() {
        for verdict in [DirectConnection.Verdict.relayOnly, .reachable] {
            let denied = DirectConnection(verdict: verdict, reasons: [.localNetworkDenied, .routerOffersNoMapping], port: 47811, lanIp: "192.168.1.20")
            XCTAssertEqual(denied.guide.map { String(localized: $0) }, [
                "Open System Settings › Privacy & Security › Local Network.",
                "Turn on Alondra (alondrad).",
                "This section updates by itself in a few seconds.",
            ])
        }
    }

    func testARouterThatRefusedTheMappingIsAskedToAllowThisMacInstead() {
        let refused = DirectConnection(verdict: .relayOnly, reasons: [.noPortMapping], portMapping: .init(upnp: true, natPmp: true, pcp: true))
        XCTAssertEqual(String(localized: refused.guide[1]),
                       "The router answers UPnP or NAT-PMP but refused this Mac: check they are fully on and not limited to some devices.")
    }

    func testTheFactsReadPlainly() {
        let offered = DirectConnection(verdict: .relayOnly, portMapping: .init(upnp: true, natPmp: true))
        XCTAssertEqual(offered.portMappingText.map { String(localized: $0) }, "Offered (UPnP, NAT-PMP), no port open")
        XCTAssertEqual(DirectConnection(verdict: .reachable, portMapping: .init(mapped: "203.0.113.7:61022")).portMappingText.map { String(localized: $0) },
                       "Open (203.0.113.7:61022)")
        XCTAssertNil(DirectConnection(verdict: .checking).portMappingText)
        XCTAssertEqual(DirectConnection(verdict: .relayOnly, sessions: .init(lan: 2, internet: 1, relay: 4)).sessionsText.map { String(localized: $0) },
                       "3 direct · 4 via the relay")
        XCTAssertNil(DirectConnection(verdict: .relayOnly).sessionsText)
        XCTAssertEqual(DirectConnection(verdict: .reachable, confirmed: true).detail.map { String(localized: $0) },
                       "A device away from home already connected directly.")
    }
}
