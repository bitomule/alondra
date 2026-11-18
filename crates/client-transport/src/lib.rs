//! How the `alondra` binary's requests reach a daemon: this Mac's over HTTP, signed with the key
//! the terminal paired; a Mac paired directly, over iroh or HTTP; or one chosen through the
//! relay. The root installs it into the client port; no adapter sees it.
#[macro_use]
extern crate alondra_client_port;

pub(crate) use alondra_client_port::client;

mod client_pair;
pub mod direct_client;
mod iroh_client;
mod local;
mod service_client;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use alondra_client_port::client::Delivery;
use alondra_client_port::transport::{self, DirectMac, Local, Remote};
use anyhow::Result;

pub use direct_client::ping;

/// The Alondra account's tokens, from the keychain the root installed.
mod account {
    use alondra_client_port::account::{AccountToken, account};
    use anyhow::Result;

    pub fn token_for_service() -> Result<AccountToken> {
        account()?.token_for_service()
    }

    pub fn ensure_token_for_enrollment() -> Result<AccountToken> {
        account()?.ensure_token_for_enrollment()
    }
}

struct Transport;

/// Installs this transport into the client port; installing it again changes nothing.
pub fn install() {
    transport::install(Arc::new(Transport));
}

impl transport::Transport for Transport {
    fn local(&self, timeout: Duration, signed: bool) -> Arc<dyn Local> {
        let route = local::LocalRoute::new(timeout);
        Arc::new(if signed { route.signed() } else { route })
    }

    fn at(&self, base: String) -> Arc<dyn Local> {
        Arc::new(local::LocalRoute::at(base, Duration::from_secs(5)))
    }

    fn relay(&self) -> Option<Arc<dyn Remote>> {
        service_client::active().map(|profile| profile as Arc<dyn Remote>)
    }

    fn relay_target(&self) -> Option<&'static str> {
        service_client::target()
    }

    fn select_relay(&self, mac: Option<&str>) -> Result<()> {
        service_client::initialize(mac)
    }

    fn relay_paired(&self) -> Result<Vec<String>> {
        service_client::paired()
    }

    fn relay_machines(&self) -> Result<Vec<String>> {
        service_client::machines()
    }

    fn connect_relay(&self, mac: &str, repair: bool) -> Result<()> {
        service_client::connect(mac, repair)
    }

    fn forget_relay(&self, mac: &str) -> Result<bool> {
        service_client::forget(mac)
    }

    fn direct_macs(&self) -> Result<Vec<Arc<dyn DirectMac>>> {
        Ok(direct_client::load()?.into_iter().map(|mac| Arc::new(direct_client::Connection::new(mac)) as Arc<dyn DirectMac>).collect())
    }

    fn connect_direct(&self, host: Option<&str>, repair: bool) -> Result<()> {
        direct_client::connect(host, repair)
    }

    fn forget_direct(&self, home: &Path, mac: &str) -> Result<Vec<String>> {
        direct_client::forget(home, mac)
    }

    fn delivery(&self, cause: &(dyn std::error::Error + 'static)) -> Option<Delivery> {
        let error = cause.downcast_ref::<ureq::Error>()?;
        Some(if local::never_left(error) { Delivery::Unsent } else { Delivery::Unanswered })
    }
}

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;
