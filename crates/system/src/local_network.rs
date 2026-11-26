//! The local network probe the daemon watches its permission with (`engine-ports`'
//! `local_network`): one datagram to the router, which macOS refuses at once with "No route to
//! host" while the Local Network permission is denied.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

/// `allowed`, `denied`, or `unknown` when there is no router to try or the send failed otherwise.
pub fn classify(sent: std::io::Result<usize>) -> &'static str {
    match sent {
        Ok(_) => "allowed",
        Err(error) if error.raw_os_error() == Some(libc::EHOSTUNREACH) => "denied",
        Err(_) => "unknown",
    }
}

fn send_to(gateway: IpAddr) -> std::io::Result<usize> {
    let unspecified: IpAddr = if gateway.is_ipv4() { Ipv4Addr::UNSPECIFIED.into() } else { Ipv6Addr::UNSPECIFIED.into() };
    UdpSocket::bind(SocketAddr::new(unspecified, 0))?.send_to(&[0], SocketAddr::new(gateway, 9))
}

pub fn probe() -> (&'static str, String) {
    let Some(router) = netwatch::interfaces::HomeRouter::new() else {
        return ("unknown", "no router found".into());
    };
    let sent = send_to(router.gateway);
    let failure = sent.as_ref().err().map(ToString::to_string).unwrap_or_default();
    (classify(sent), failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_send_means_denied_and_other_failures_stay_unknown() {
        assert_eq!(classify(Ok(1)), "allowed");
        assert_eq!(classify(Err(std::io::Error::from_raw_os_error(libc::EHOSTUNREACH))), "denied");
        assert_eq!(classify(Err(std::io::Error::from_raw_os_error(libc::ENETUNREACH))), "unknown");
        assert_eq!(classify(Err(std::io::ErrorKind::PermissionDenied.into())), "unknown");
    }
}
