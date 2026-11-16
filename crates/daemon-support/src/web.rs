//! Every HTTP client of the daemon is built here, so every host it looks up goes through
//! `alondra_core::outside` first: in an isolated world the lookup is refused and written down as a
//! leak before any connection is made.

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::sync::Arc;

pub fn client() -> reqwest::ClientBuilder {
    reqwest::Client::builder().dns_resolver(Arc::new(ThroughTheDoor))
}

struct ThroughTheDoor;

impl Resolve for ThroughTheDoor {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let admitted = alondra_core::outside::connect(&host);
        Box::pin(async move {
            admitted?;
            let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_client_looks_up_hosts_as_the_system_does() {
        let addresses: Vec<_> = ThroughTheDoor.resolve("localhost".parse().unwrap()).await.unwrap().collect();
        assert!(addresses.iter().all(|address| address.ip().is_loopback() && address.port() == 0), "{addresses:?}");
        assert!(!addresses.is_empty());
    }
}
