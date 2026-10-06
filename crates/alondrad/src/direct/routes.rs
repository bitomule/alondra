//! Where a paired device can reach this Mac directly: its addresses on the local network and,
//! when Tailscale runs, its tailnet address and name. Devices read this through an encrypted
//! envelope (`GET /routes`), so a device paired through the relay also learns the direct path.
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

pub(crate) const PATH: &str = "/routes";

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct Routes {
    pub(crate) port: u16,
    pub(crate) lan: Vec<IpAddr>,
    pub(crate) tailscale: Option<Tailscale>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct Tailscale {
    pub(crate) ip: IpAddr,
    pub(crate) name: Option<String>,
}

pub(crate) fn tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]),
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

fn lan(ip: IpAddr) -> bool {
    if tailnet(ip) {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// Splits this Mac's interface addresses into LAN and tailnet ones. IPv4 first: it is what
/// clients try first and what iOS's local-networking exception covers by address.
pub(crate) fn classify(addresses: &[IpAddr], tailnet_name: Option<String>, port: u16) -> Routes {
    let mut lan_addresses: Vec<IpAddr> = addresses.iter().copied().filter(|ip| lan(*ip)).collect();
    lan_addresses.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    lan_addresses.dedup();
    let tailscale = addresses
        .iter()
        .copied()
        .find(|ip| ip.is_ipv4() && tailnet(*ip))
        .map(|ip| Tailscale { ip, name: tailnet_name });
    Routes { port, lan: lan_addresses, tailscale }
}

fn interface_addresses() -> Vec<IpAddr> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list we only read and then free once.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return found;
    }
    let mut cursor = list;
    while !cursor.is_null() {
        // SAFETY: `cursor` walks the list getifaddrs returned; each node stays valid until freeifaddrs.
        let entry = unsafe { &*cursor };
        let up = entry.ifa_flags & (libc::IFF_UP as u32) != 0;
        if up && !entry.ifa_addr.is_null() {
            // SAFETY: ifa_addr is non-null and its family says which sockaddr layout it holds.
            match i32::from(unsafe { (*entry.ifa_addr).sa_family }) {
                libc::AF_INET => {
                    let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                    found.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr))));
                }
                libc::AF_INET6 => {
                    let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
                    found.push(IpAddr::V6(std::net::Ipv6Addr::from(addr.sin6_addr.s6_addr)));
                }
                _ => {}
            }
        }
        cursor = entry.ifa_next;
    }
    // SAFETY: frees the list getifaddrs allocated, exactly once.
    unsafe { libc::freeifaddrs(list) };
    found
}

/// The MagicDNS name Tailscale gives this Mac, when its CLI answers quickly.
async fn tailnet_name() -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("tailscale").args(["status", "--json"]).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    status["Self"]["DNSName"]
        .as_str()
        .map(|name| name.trim_end_matches('.').to_owned())
        .filter(|name| !name.is_empty())
}

/// The addresses the listener bound on `listen` actually answers at.
pub(crate) fn reachable(addresses: Vec<IpAddr>, listen: IpAddr) -> Vec<IpAddr> {
    match listen {
        IpAddr::V4(v4) if v4.is_unspecified() => addresses.into_iter().filter(IpAddr::is_ipv4).collect(),
        IpAddr::V6(v6) if v6.is_unspecified() => addresses,
        bound => addresses.into_iter().filter(|ip| *ip == bound).collect(),
    }
}

pub(crate) async fn current(port: u16, listen: IpAddr) -> Routes {
    let addresses = reachable(interface_addresses(), listen);
    let name = if addresses.iter().any(|ip| tailnet(*ip)) { tailnet_name().await } else { None };
    classify(&addresses, name, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lan_and_tailnet_and_drops_public_and_loopback() {
        let addresses: Vec<IpAddr> = [
            "127.0.0.1", "192.168.1.20", "8.8.8.8", "100.101.102.103", "10.0.0.5", "fd12:3456::1", "fd7a:115c:a1e0::9", "::1",
        ]
        .iter()
        .map(|a| a.parse().unwrap())
        .collect();
        let routes = classify(&addresses, Some("mac.tail1234.ts.net".into()), 47810);
        assert_eq!(
            routes.lan,
            ["10.0.0.5", "192.168.1.20", "fd12:3456::1"].iter().map(|a| a.parse::<IpAddr>().unwrap()).collect::<Vec<_>>()
        );
        assert_eq!(
            routes.tailscale,
            Some(Tailscale { ip: "100.101.102.103".parse().unwrap(), name: Some("mac.tail1234.ts.net".into()) })
        );
    }

    #[test]
    fn only_addresses_the_listener_answers_at_are_offered() {
        let all: Vec<IpAddr> = ["192.168.1.20", "fd12::1", "10.0.0.5"].iter().map(|a| a.parse().unwrap()).collect();
        let v4: Vec<IpAddr> = ["192.168.1.20", "10.0.0.5"].iter().map(|a| a.parse().unwrap()).collect();
        assert_eq!(reachable(all.clone(), "0.0.0.0".parse().unwrap()), v4);
        assert_eq!(reachable(all.clone(), "::".parse().unwrap()), all);
        assert_eq!(reachable(all, "10.0.0.5".parse().unwrap()), vec!["10.0.0.5".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn the_host_lists_at_least_its_own_interfaces() {
        assert!(!interface_addresses().is_empty());
    }
}
