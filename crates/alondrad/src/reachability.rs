//! Whether a device away from home can reach this Mac directly, why not, and which facts the
//! apps and `alondra doctor` show next to a router guide. A phone on a mobile network usually sits
//! behind a symmetric NAT, which changes its public port for every destination; a direct path to
//! it only opens when the Mac has a way in: a port the router maps (UPnP, NAT-PMP, PCP) or that
//! is forwarded by hand, a public IPv6 address, or no NAT at all. The daemon reports codes; the
//! apps and the CLI word them.
use iroh::endpoint::{Connection, PortmapperConfig};
use iroh::unstable_net_report::NetReport;
use iroh::{Endpoint, TransportAddr, Watcher};
use serde::Serialize;
use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Mutex;
use std::time::Duration;

const PROBE_EVERY: Duration = Duration::from_secs(10 * 60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const KEPT_SESSIONS: usize = 50;
const REFRESH: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NetFacts {
    pub(crate) udp_v4: bool,
    pub(crate) udp_v6: bool,
    pub(crate) global_v4: Option<SocketAddrV4>,
    pub(crate) global_v6: Option<SocketAddrV6>,
    pub(crate) varies_v4: Option<bool>,
    pub(crate) varies_v6: Option<bool>,
    pub(crate) captive_portal: bool,
}

impl From<&NetReport> for NetFacts {
    fn from(report: &NetReport) -> Self {
        Self {
            udp_v4: report.udp_v4,
            udp_v6: report.udp_v6,
            global_v4: report.global_v4,
            global_v6: report.global_v6,
            varies_v4: report.mapping_varies_by_dest_ipv4,
            varies_v6: report.mapping_varies_by_dest_ipv6,
            captive_portal: report.captive_portal == Some(true),
        }
    }
}

/// Which port mapping protocols the router answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Offered {
    pub(crate) upnp: bool,
    pub(crate) nat_pmp: bool,
    pub(crate) pcp: bool,
}

impl Offered {
    fn any(self) -> bool {
        self.upnp || self.nat_pmp || self.pcp
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PathKind {
    Lan,
    Internet,
    Relay,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Sessions {
    pub(crate) lan: usize,
    pub(crate) internet: usize,
    pub(crate) relay: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Inputs {
    /// `None` until iroh's first net report, which needs the relay.
    pub(crate) report: Option<NetFacts>,
    /// `None` until the router was probed.
    pub(crate) offered: Option<Offered>,
    /// The endpoint's own IP addresses: interfaces, the public one iroh saw, and a mapped one.
    pub(crate) endpoint_addrs: Vec<SocketAddr>,
    /// iroh got an external address from the router's port mapping. With an easy NAT the mapped
    /// address can be the very one the relay sees, so the addresses alone cannot tell.
    pub(crate) portmapped: bool,
    /// This Mac's interface addresses.
    pub(crate) interface_ips: Vec<IpAddr>,
    pub(crate) gateway: Option<IpAddr>,
    pub(crate) port: Option<u16>,
    /// Whether the UDP port stays the same across restarts, so a forwarded port keeps working.
    pub(crate) port_fixed: bool,
    pub(crate) relay_configured: bool,
    pub(crate) relay_connected: bool,
    /// The iroh relay resolves to an address on this Mac's own network.
    pub(crate) relay_on_same_network: bool,
    pub(crate) local_network: &'static str,
    pub(crate) sessions: Sessions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    /// The Mac has a way in, so a direct path opens even from a mobile network.
    Reachable,
    /// No way in: away from home the connection goes through the relay.
    RelayOnly,
    Checking,
    /// Without a relay the Mac cannot learn how it looks from outside.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    LocalNetworkDenied,
    RelayOff,
    RelayDisconnected,
    CaptivePortal,
    RelayOnSameNetwork,
    NoUdp,
    SymmetricNat,
    RouterOffersNoMapping,
    NoPortMapping,
    NoIpv6,
    PortNotFixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Nat {
    None,
    Easy,
    Symmetric,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PortMapping {
    #[serde(flatten)]
    pub(crate) offered: Offered,
    pub(crate) mapped: Option<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Reachability {
    pub(crate) verdict: Verdict,
    /// A device away from home has already connected directly.
    pub(crate) confirmed: bool,
    pub(crate) reasons: Vec<Reason>,
    pub(crate) port_mapping: Option<PortMapping>,
    pub(crate) public_ipv4: Option<Ipv4Addr>,
    pub(crate) public_ipv6: Option<std::net::Ipv6Addr>,
    pub(crate) nat: Nat,
    pub(crate) port: Option<u16>,
    pub(crate) port_fixed: bool,
    pub(crate) lan_ip: Option<IpAddr>,
    pub(crate) sessions: Sessions,
}

/// Public unicast: not private, loopback, link-local, CGNAT (also Tailscale) or documentation.
fn public_v4(ip: Ipv4Addr) -> bool {
    let cgnat = ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64;
    !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || cgnat)
}

pub(crate) fn on_this_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !public_v4(ip),
        IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

fn mapped(inputs: &Inputs, observed: Option<SocketAddrV4>) -> Option<SocketAddr> {
    let distinct = inputs.endpoint_addrs.iter().copied().find(|addr| match addr {
        SocketAddr::V4(v4) => {
            public_v4(*v4.ip()) && Some(*v4) != observed && !inputs.interface_ips.contains(&IpAddr::V4(*v4.ip()))
        }
        SocketAddr::V6(_) => false,
    });
    distinct.or(observed.filter(|_| inputs.portmapped).map(SocketAddr::V4))
}

fn lan_ip(inputs: &Inputs) -> Option<IpAddr> {
    let candidates: Vec<Ipv4Addr> = inputs
        .interface_ips
        .iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(v4) if v4.is_private() => Some(*v4),
            _ => None,
        })
        .collect();
    let same_subnet = |ip: &&Ipv4Addr| match inputs.gateway {
        Some(IpAddr::V4(gateway)) => ip.octets()[..3] == gateway.octets()[..3],
        _ => false,
    };
    candidates.iter().find(same_subnet).or(candidates.first()).map(|ip| IpAddr::V4(*ip))
}

pub(crate) fn diagnose(inputs: &Inputs) -> Reachability {
    let report = inputs.report.clone();
    let observed = report.as_ref().and_then(|report| report.global_v4);
    let mapped = mapped(inputs, observed);
    let public_ipv4 = observed.map(|addr| *addr.ip());
    let public_ipv6 = report.as_ref().filter(|report| report.udp_v6).and_then(|report| report.global_v6).map(|addr| *addr.ip());
    let nat = match &report {
        _ if public_ipv4.is_some_and(|ip| inputs.interface_ips.contains(&IpAddr::V4(ip))) => Nat::None,
        Some(report) if report.varies_v4 == Some(true) => Nat::Symmetric,
        Some(report) if report.varies_v4 == Some(false) => Nat::Easy,
        _ => Nat::Unknown,
    };
    let ipv6_way_in = public_ipv6.is_some() && report.as_ref().is_some_and(|report| report.varies_v6 != Some(true));
    let confirmed = inputs.sessions.internet > 0;
    let way_in = mapped.is_some() || ipv6_way_in || (nat == Nat::None && report.as_ref().is_some_and(|r| r.udp_v4));
    let verdict = match &report {
        _ if way_in || confirmed => Verdict::Reachable,
        Some(_) => Verdict::RelayOnly,
        None if !inputs.relay_configured || !inputs.relay_connected => Verdict::Unknown,
        None => Verdict::Checking,
    };
    let mut reasons = Vec::new();
    if inputs.local_network == "denied" {
        reasons.push(Reason::LocalNetworkDenied);
    }
    if verdict != Verdict::Reachable {
        let measured = report.is_some();
        let report = report.unwrap_or_default();
        let checks = [
            (!inputs.relay_configured, Reason::RelayOff),
            (inputs.relay_configured && !inputs.relay_connected, Reason::RelayDisconnected),
            (report.captive_portal, Reason::CaptivePortal),
            (inputs.relay_on_same_network, Reason::RelayOnSameNetwork),
            (measured && !inputs.relay_on_same_network && !report.udp_v4 && !report.udp_v6, Reason::NoUdp),
            (nat == Nat::Symmetric, Reason::SymmetricNat),
            (inputs.offered.is_some_and(|offered| !offered.any()), Reason::RouterOffersNoMapping),
            (inputs.offered.is_some_and(Offered::any), Reason::NoPortMapping),
            (verdict == Verdict::RelayOnly && public_ipv6.is_none(), Reason::NoIpv6),
            (inputs.port.is_some() && !inputs.port_fixed, Reason::PortNotFixed),
        ];
        reasons.extend(checks.into_iter().filter(|(applies, _)| *applies).map(|(_, reason)| reason));
    }
    Reachability {
        verdict,
        confirmed,
        reasons,
        port_mapping: inputs.offered.map(|offered| PortMapping { offered, mapped }),
        public_ipv4,
        public_ipv6,
        nat,
        port: inputs.port,
        port_fixed: inputs.port_fixed,
        lan_ip: lan_ip(inputs),
        sessions: inputs.sessions,
    }
}

/// The path a connection settled on: the best one it had selected while it was open.
pub(crate) fn path_kind(remote: &TransportAddr) -> PathKind {
    match remote {
        TransportAddr::Ip(addr) if on_this_network(addr.ip()) => PathKind::Lan,
        TransportAddr::Ip(_) => PathKind::Internet,
        _ => PathKind::Relay,
    }
}

fn rank(kind: PathKind) -> u8 {
    match kind {
        PathKind::Relay => 0,
        PathKind::Internet => 1,
        PathKind::Lan => 2,
    }
}

/// Follows a connection until it closes and returns the best path it used.
pub(crate) async fn best_path(connection: Connection) -> Option<PathKind> {
    use n0_future::StreamExt as _;
    let mut best: Option<PathKind> = None;
    let mut paths = connection.paths_stream();
    while let Some(list) = paths.next().await {
        for path in list.iter().filter(|path| path.is_selected()) {
            let kind = path_kind(path.remote_addr());
            if best.is_none_or(|best| rank(kind) > rank(best)) {
                best = Some(kind);
            }
        }
    }
    best
}

#[derive(Default)]
pub(crate) struct Ledger(VecDeque<PathKind>);

impl Ledger {
    pub(crate) fn record(&mut self, kind: PathKind) {
        if self.0.len() == KEPT_SESSIONS {
            self.0.pop_front();
        }
        self.0.push_back(kind);
    }

    pub(crate) fn sessions(&self) -> Sessions {
        let count = |kind| self.0.iter().filter(|seen| **seen == kind).count();
        Sessions { lan: count(PathKind::Lan), internet: count(PathKind::Internet), relay: count(PathKind::Relay) }
    }
}

struct Live {
    inputs: Inputs,
    ledger: Ledger,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn change(update: impl FnOnce(&mut Live)) {
    let mut guard = LIVE.lock().expect("reachability poisoned");
    let live = guard.get_or_insert_with(|| Live { inputs: Inputs::default(), ledger: Ledger::default() });
    update(live);
    live.inputs.sessions = live.ledger.sessions();
}

pub(crate) fn record_session(kind: PathKind) {
    #[cfg(test)]
    RECORDED[rank(kind) as usize].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    change(|live| live.ledger.record(kind));
}

#[cfg(test)]
static RECORDED: [std::sync::atomic::AtomicUsize; 3] = [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

/// Every session recorded in this test process, which the capped ledger cannot tell.
#[cfg(test)]
pub(crate) fn recorded(kind: PathKind) -> usize {
    RECORDED[rank(kind) as usize].load(std::sync::atomic::Ordering::SeqCst)
}

/// The latest diagnosis, or `None` while the endpoint is not up.
pub(crate) fn current() -> Option<Reachability> {
    let inputs = LIVE.lock().expect("reachability poisoned").as_ref()?.inputs.clone();
    let (relay, relay_connected) = crate::iroh_endpoint::relay_state();
    Some(diagnose(&Inputs {
        relay_configured: relay.is_some(),
        relay_connected,
        local_network: crate::local_network::status(),
        ..inputs
    }))
}

async fn probe_router() -> Option<Offered> {
    let client = portmapper::Client::new(portmapper::Config::default());
    match tokio::time::timeout(PROBE_TIMEOUT, client.probe()).await {
        Ok(Ok(Ok(found))) => Some(Offered { upnp: found.upnp, nat_pmp: found.nat_pmp, pcp: found.pcp }),
        Ok(Ok(Err(error))) if matches!(error, portmapper::ProbeError::NoGateway { .. }) => None,
        _ => Some(Offered::default()),
    }
}

async fn relay_on_same_network(url: &str, interface_ips: &[IpAddr]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return false };
    let Some(host) = parsed.host_str() else { return false };
    let port = parsed.port_or_known_default().unwrap_or(443);
    match tokio::net::lookup_host((host, port)).await {
        Ok(found) => found.map(|addr| addr.ip()).any(|ip| on_this_network(ip) || interface_ips.contains(&ip)),
        Err(_) => false,
    }
}

async fn interfaces() -> (Vec<IpAddr>, Option<IpAddr>) {
    let state = netwatch::interfaces::State::new().await;
    let gateway = netwatch::interfaces::HomeRouter::new().map(|router| router.gateway);
    (state.local_addresses.regular, gateway)
}

/// Keeps the diagnosis current for as long as the endpoint runs: on every net report and
/// address change, and with a router probe now and every ten minutes.
pub(crate) async fn watch(endpoint: Endpoint, port_fixed: bool, portmapper: &PortmapperConfig) {
    let port = endpoint.bound_sockets().iter().find(|addr| addr.is_ipv4()).map(SocketAddr::port);
    change(|live| {
        live.inputs.port = port;
        live.inputs.port_fixed = port_fixed;
    });
    let probing = !matches!(portmapper, PortmapperConfig::Disabled);
    let mut reports = endpoint.net_report();
    let mut addrs = endpoint.watch_addr();
    let mut probe = tokio::time::interval(PROBE_EVERY);
    let mut refresh = tokio::time::interval(REFRESH);
    let mut logged: Option<Reachability> = None;
    loop {
        let (interface_ips, gateway) = interfaces().await;
        let (relay, _) = crate::iroh_endpoint::relay_state();
        let same_network = match &relay {
            Some(url) => relay_on_same_network(url, &interface_ips).await,
            None => false,
        };
        change(|live| {
            live.inputs.report = reports.get().as_ref().map(NetFacts::from);
            live.inputs.endpoint_addrs = addrs.get().ip_addrs().copied().collect();
            live.inputs.interface_ips = interface_ips;
            live.inputs.gateway = gateway;
            live.inputs.relay_on_same_network = same_network;
            live.inputs.portmapped = endpoint.metrics().net_report.portmap_external_address_updated.get() > 0;
        });
        if let Some(now) = current().filter(|now| logged.as_ref().is_none_or(|was| (was.verdict, &was.reasons) != (now.verdict, &now.reasons))) {
            log!("direct connection: {:?} {:?}", now.verdict, now.reasons);
            logged = Some(now);
        }
        tokio::select! {
            changed = reports.updated() => if changed.is_err() { return },
            changed = addrs.updated() => if changed.is_err() { return },
            _ = refresh.tick() => {}
            _ = probe.tick(), if probing => {
                let offered = probe_router().await;
                change(|live| live.inputs.offered = offered);
            }
        }
    }
}

#[cfg(test)]
#[path = "reachability_tests.rs"]
mod tests;
