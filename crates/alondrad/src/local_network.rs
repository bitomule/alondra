//! Whether macOS lets alondrad send to the local network. A launchd agent needs the Local Network
//! permission to send UDP to a LAN address (TN3179), which the direct iroh path over the same
//! Wi-Fi does; accepting the TCP of the direct listener does not need it. One datagram to the
//! router tells: macOS answers a denied send with "No route to host" at once, and the first
//! attempt is also what makes it ask the user.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Mutex;

static STATUS: Mutex<&str> = Mutex::new("unknown");

pub(crate) fn status() -> &'static str {
    *STATUS.lock().expect("local network status poisoned")
}

/// `allowed`, `denied`, or `unknown` when there is no router to try or the send failed otherwise.
pub(crate) fn classify(sent: std::io::Result<usize>) -> &'static str {
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

pub(crate) fn check() -> &'static str {
    let Some(router) = netwatch::interfaces::HomeRouter::new() else {
        log!("local network: no router found to check the Local Network permission");
        return "unknown";
    };
    let sent = send_to(router.gateway);
    let failure = sent.as_ref().err().map(ToString::to_string).unwrap_or_default();
    let found = classify(sent);
    match found {
        "allowed" => log!("local network: allowed"),
        "denied" => log!("local network: denied; allow Local Network for alondrad in System Settings, Privacy & Security"),
        _ => log!("local network: unknown ({failure})"),
    }
    *STATUS.lock().expect("local network status poisoned") = found;
    found
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
