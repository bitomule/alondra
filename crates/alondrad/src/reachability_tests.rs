use super::*;
use serde_json::json;

const HOME_PUBLIC: Ipv4Addr = Ipv4Addr::new(81, 40, 12, 7);
const MAC_LAN: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

fn v4(ip: Ipv4Addr, port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(ip, port)
}

/// The home network of the October measurements: an easy NAT that keeps the port, no IPv6,
/// the relay outside, and a router that answers no port mapping protocol.
fn home() -> Inputs {
    Inputs {
        report: Some(NetFacts {
            udp_v4: true,
            global_v4: Some(v4(HOME_PUBLIC, 47811)),
            varies_v4: Some(false),
            ..NetFacts::default()
        }),
        offered: Some(Offered::default()),
        endpoint_addrs: vec![SocketAddr::V4(v4(MAC_LAN, 47811)), SocketAddr::V4(v4(HOME_PUBLIC, 47811))],
        interface_ips: vec![IpAddr::V4(MAC_LAN), IpAddr::V4(Ipv4Addr::new(100, 101, 7, 9))],
        gateway: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
        port: Some(47811),
        port_fixed: true,
        relay_configured: true,
        relay_connected: true,
        local_network: "allowed",
        ..Inputs::default()
    }
}

#[test]
fn a_home_router_without_port_mapping_leaves_the_mac_on_the_relay_and_says_why() {
    let found = diagnose(&home());
    assert_eq!(found.verdict, Verdict::RelayOnly);
    assert_eq!(found.reasons, vec![Reason::RouterOffersNoMapping, Reason::NoIpv6]);
    assert_eq!(found.nat, Nat::Easy);
    assert_eq!(found.public_ipv4, Some(HOME_PUBLIC));
    assert_eq!(found.port_mapping, Some(PortMapping { offered: Offered::default(), mapped: None }));
    assert_eq!(found.lan_ip, Some(IpAddr::V4(MAC_LAN)));
    assert!(!found.confirmed);
}

#[test]
fn a_port_the_router_mapped_makes_the_mac_reachable() {
    let mut inputs = home();
    inputs.offered = Some(Offered { upnp: true, ..Offered::default() });
    let mapping = SocketAddr::V4(v4(HOME_PUBLIC, 61022));
    inputs.endpoint_addrs.push(mapping);
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::Reachable);
    assert_eq!(found.reasons, vec![]);
    assert_eq!(found.port_mapping.unwrap().mapped, Some(mapping));
}

#[test]
fn a_mapping_on_the_same_port_the_relay_sees_still_counts_as_open() {
    let mut inputs = home();
    inputs.offered = Some(Offered { upnp: true, nat_pmp: true, pcp: true });
    inputs.portmapped = true;
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::Reachable);
    assert_eq!(found.port_mapping.unwrap().mapped, Some(SocketAddr::V4(v4(HOME_PUBLIC, 47811))));
}

#[test]
fn a_router_that_offers_mapping_but_gave_none_is_told_apart_from_one_that_offers_none() {
    let mut inputs = home();
    inputs.offered = Some(Offered { nat_pmp: true, ..Offered::default() });
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::RelayOnly);
    assert_eq!(found.reasons, vec![Reason::NoPortMapping, Reason::NoIpv6]);
}

#[test]
fn a_public_ipv6_address_with_udp_is_a_way_in() {
    let mut inputs = home();
    let report = inputs.report.as_mut().unwrap();
    report.udp_v6 = true;
    report.global_v6 = Some(SocketAddrV6::new("2a0c:5a80::7".parse().unwrap(), 47811, 0, 0));
    report.varies_v6 = Some(false);
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::Reachable);
    assert_eq!(found.public_ipv6, Some("2a0c:5a80::7".parse().unwrap()));
}

#[test]
fn an_ipv6_address_without_udp_is_not_shown_as_a_way_in() {
    let mut inputs = home();
    inputs.report.as_mut().unwrap().global_v6 = Some(SocketAddrV6::new("2a0c:5a80::7".parse().unwrap(), 47811, 0, 0));
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::RelayOnly);
    assert_eq!(found.public_ipv6, None);
}

#[test]
fn a_relay_on_the_same_network_explains_why_the_mac_sees_no_public_address() {
    let mut inputs = home();
    inputs.report = Some(NetFacts::default());
    inputs.endpoint_addrs = vec![SocketAddr::V4(v4(MAC_LAN, 47811))];
    inputs.relay_on_same_network = true;
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::RelayOnly);
    assert_eq!(found.reasons, vec![Reason::RelayOnSameNetwork, Reason::RouterOffersNoMapping, Reason::NoIpv6]);
    assert_eq!(found.nat, Nat::Unknown);
}

#[test]
fn no_udp_at_all_is_its_own_reason_when_the_relay_is_elsewhere() {
    let mut inputs = home();
    inputs.report = Some(NetFacts::default());
    inputs.endpoint_addrs = vec![SocketAddr::V4(v4(MAC_LAN, 47811))];
    let found = diagnose(&inputs);
    assert_eq!(found.reasons[0], Reason::NoUdp);
}

#[test]
fn a_nat_that_changes_the_port_per_destination_is_called_symmetric() {
    let mut inputs = home();
    inputs.report.as_mut().unwrap().varies_v4 = Some(true);
    let found = diagnose(&inputs);
    assert_eq!(found.nat, Nat::Symmetric);
    assert_eq!(found.reasons, vec![Reason::SymmetricNat, Reason::RouterOffersNoMapping, Reason::NoIpv6]);
}

#[test]
fn a_public_address_on_the_mac_itself_means_no_nat() {
    let mut inputs = home();
    inputs.interface_ips.push(IpAddr::V4(HOME_PUBLIC));
    let found = diagnose(&inputs);
    assert_eq!(found.nat, Nat::None);
    assert_eq!(found.verdict, Verdict::Reachable);
    assert_eq!(found.port_mapping.unwrap().mapped, None, "its own address is not a mapping");
}

#[test]
fn a_tailscale_address_is_not_taken_for_a_mapping() {
    let mut inputs = home();
    inputs.endpoint_addrs.push(SocketAddr::V4(v4(Ipv4Addr::new(100, 101, 7, 9), 47811)));
    assert_eq!(diagnose(&inputs).verdict, Verdict::RelayOnly);
}

#[test]
fn a_device_that_already_connected_directly_from_outside_confirms_the_mac_is_reachable() {
    let mut inputs = home();
    inputs.sessions = Sessions { lan: 2, internet: 1, relay: 3, ..Sessions::default() };
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::Reachable);
    assert!(found.confirmed);
    assert_eq!(found.sessions, inputs.sessions);
}

#[test]
fn a_denied_local_network_permission_is_listed_even_when_the_mac_is_reachable() {
    let mut inputs = home();
    inputs.interface_ips.push(IpAddr::V4(HOME_PUBLIC));
    inputs.local_network = "denied";
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::Reachable);
    assert_eq!(found.reasons, vec![Reason::LocalNetworkDenied]);
}

#[test]
fn before_the_first_report_it_is_checking_with_the_relay_and_unknown_without_it() {
    let mut inputs = home();
    inputs.report = None;
    inputs.offered = None;
    inputs.endpoint_addrs.truncate(1);
    assert_eq!(diagnose(&inputs).verdict, Verdict::Checking);
    assert_eq!(diagnose(&inputs).reasons, vec![]);

    inputs.relay_connected = false;
    let found = diagnose(&inputs);
    assert_eq!(found.verdict, Verdict::Unknown);
    assert_eq!(found.reasons, vec![Reason::RelayDisconnected]);

    inputs.relay_configured = false;
    assert_eq!(diagnose(&inputs).reasons, vec![Reason::RelayOff]);
}

#[test]
fn a_port_that_changes_on_every_start_is_a_reason_since_forwarding_it_would_not_last() {
    let mut inputs = home();
    inputs.port_fixed = false;
    assert_eq!(diagnose(&inputs).reasons.last(), Some(&Reason::PortNotFixed));
}

#[test]
fn the_lan_address_is_the_one_beside_the_router() {
    let mut inputs = home();
    inputs.interface_ips.insert(0, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4)));
    assert_eq!(diagnose(&inputs).lan_ip, Some(IpAddr::V4(MAC_LAN)));
    inputs.gateway = None;
    assert_eq!(diagnose(&inputs).lan_ip, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4))));
}

#[test]
fn paths_are_classified_as_lan_internet_or_relay() {
    assert_eq!(path_kind(&TransportAddr::Ip("192.168.1.30:5000".parse().unwrap())), PathKind::Lan);
    assert_eq!(path_kind(&TransportAddr::Ip("[fe80::1]:5000".parse().unwrap())), PathKind::Lan);
    assert_eq!(path_kind(&TransportAddr::Ip("100.101.7.9:5000".parse().unwrap())), PathKind::Lan);
    assert_eq!(path_kind(&TransportAddr::Ip("88.1.2.3:5000".parse().unwrap())), PathKind::Internet);
    assert_eq!(path_kind(&TransportAddr::Ip("[2a0c:5a80::9]:5000".parse().unwrap())), PathKind::Internet);
    assert_eq!(path_kind(&TransportAddr::Relay("https://relay.example.com".parse().unwrap())), PathKind::Relay);
}

#[test]
fn the_ledger_counts_only_the_latest_sessions() {
    let mut ledger = Ledger::default();
    for _ in 0..KEPT_SESSIONS {
        ledger.record(PathKind::Relay, Bytes { relay: 1_000, ..Bytes::default() });
    }
    ledger.record(PathKind::Internet, Bytes { internet: 30, relay: 5, ..Bytes::default() });
    ledger.record(PathKind::Lan, Bytes { lan: 7, ..Bytes::default() });
    assert_eq!(
        ledger.sessions(),
        Sessions {
            lan: 1,
            internet: 1,
            relay: KEPT_SESSIONS - 2,
            bytes: Bytes { lan: 7, internet: 30, relay: 5 + 1_000 * (KEPT_SESSIONS as u64 - 2) },
        }
    );
}

#[test]
fn a_path_counts_its_latest_total_once_and_each_kind_adds_up_its_paths() {
    let mut tally = Tally::new();
    tally.count(0, PathKind::Relay, 1_200);
    tally.count(0, PathKind::Relay, 4_800);
    tally.count(1, PathKind::Lan, 300);
    tally.count(2, PathKind::Lan, 50);
    tally.count(1, PathKind::Lan, 900);
    assert_eq!(tally.total(), Bytes { lan: 950, internet: 0, relay: 4_800 });
}

#[test]
fn the_apps_read_this_exact_shape() {
    let mut inputs = home();
    inputs.offered = Some(Offered { upnp: true, ..Offered::default() });
    inputs.endpoint_addrs.push(SocketAddr::V4(v4(HOME_PUBLIC, 61022)));
    assert_eq!(
        serde_json::to_value(diagnose(&inputs)).unwrap(),
        json!({
            "verdict": "reachable", "confirmed": false, "reasons": [],
            "port_mapping": {"upnp": true, "nat_pmp": false, "pcp": false, "mapped": "81.40.12.7:61022"},
            "public_ipv4": "81.40.12.7", "public_ipv6": null, "nat": "easy",
            "port": 47811, "port_fixed": true, "lan_ip": "192.168.1.20",
            "sessions": {"lan": 0, "internet": 0, "relay": 0, "bytes": {"lan": 0, "internet": 0, "relay": 0}},
        })
    );
    let mut checking = home();
    checking.report = None;
    checking.offered = None;
    checking.endpoint_addrs.truncate(1);
    checking.relay_connected = false;
    checking.local_network = "denied";
    let reasons = serde_json::to_value(diagnose(&checking).reasons).unwrap();
    assert_eq!(reasons, json!(["local_network_denied", "relay_disconnected"]));
    for reason in [Reason::RelayOff, Reason::CaptivePortal, Reason::RelayOnSameNetwork, Reason::NoUdp, Reason::SymmetricNat,
        Reason::RouterOffersNoMapping, Reason::NoPortMapping, Reason::NoIpv6, Reason::PortNotFixed] {
        let code = serde_json::to_value(reason).unwrap();
        assert!(code.as_str().is_some_and(|code| code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')), "{code}");
    }
}
