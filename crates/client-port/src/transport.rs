//! How a request reaches a daemon. The root installs one transport for the whole process; a
//! `Client` asks it for its route and never knows which one it got.
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::client::Delivery;
use crate::client::availability::ResponseObserver;

/// One request to this Mac's daemon over HTTP.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub content_type: &'a str,
    pub body: &'a [u8],
    /// The MCP operation the request belongs to, which the daemon reads from its header.
    pub operation: Option<&'a str>,
}

/// This Mac's daemon over HTTP: the route of every request unless a remote Mac was chosen, and
/// the only one for files, uploads and a node's domain calls.
pub trait Local: Send + Sync {
    /// The answer's status and body; `observer` hears of the answer as soon as it starts.
    fn send(&self, request: Request<'_>, observer: Option<&ResponseObserver>) -> Result<(u16, Vec<u8>)>;
    /// A producer's read with its daemon-issued bearer, never the user's key.
    fn domain_get(&self, path: &str, bearer: &str) -> Result<(u16, Vec<u8>)>;
    /// A producer's request with its daemon-issued bearer, and the daemon's version when it says it.
    fn domain_call(&self, request: Request<'_>, bearer: &str) -> Result<(u16, Option<String>, Vec<u8>)>;
    /// The same route, each request bounded by `timeout`.
    fn bounded(&self, timeout: Duration) -> Arc<dyn Local>;
}

/// A Mac reached from afar: through the relay or directly.
pub trait Remote: Send + Sync {
    fn request(&self, method: &str, path: &str, body: Option<&Value>, timeout: Duration, observer: Option<&ResponseObserver>) -> Result<(u16, Value)>;
}

/// How a route reaches its Mac, for the connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Lan,
    Tailscale,
    /// iroh, straight to the Mac's public address.
    Direct,
    /// iroh, through the iroh relay.
    Relay,
}

/// A Mac this terminal paired with directly.
pub trait DirectMac: Remote {
    fn mac_id(&self) -> &str;
    fn name(&self) -> &str;
    /// How the route in use reaches the Mac, once one answered.
    fn route_kind(&self) -> Option<RouteKind>;
}

/// The routes to the daemons this terminal knows, and the Macs it paired with.
pub trait Transport: Send + Sync {
    /// This Mac's daemon; `signed` signs the requests as the user, pairing on first use.
    fn local(&self, timeout: Duration, signed: bool) -> Arc<dyn Local>;
    /// The daemon at `base`, unsigned: tests run a fake one there.
    fn at(&self, base: String) -> Arc<dyn Local>;
    /// The Mac chosen through the relay for this process, if one was.
    fn relay(&self) -> Option<Arc<dyn Remote>>;
    /// The id of the Mac chosen through the relay.
    fn relay_target(&self) -> Option<&'static str>;
    /// Chooses the Mac `mac` names through the relay for the rest of this process.
    fn select_relay(&self, mac: Option<&str>) -> Result<()>;
    /// The Macs this terminal paired with through the relay, by id.
    fn relay_paired(&self) -> Result<Vec<String>>;
    /// The Macs the signed-in account has, by id.
    fn relay_machines(&self) -> Result<Vec<String>>;
    fn connect_relay(&self, mac: &str, repair: bool) -> Result<()>;
    /// Whether the Mac was paired through the relay, now forgotten.
    fn forget_relay(&self, mac: &str) -> Result<bool>;
    /// The Macs this terminal paired with directly.
    fn direct_macs(&self) -> Result<Vec<Arc<dyn DirectMac>>>;
    /// Pairs with a Mac found on this network, or at `host`.
    fn connect_direct(&self, host: Option<&str>, repair: bool) -> Result<()>;
    /// The ids of the direct Macs `mac` named, now forgotten.
    fn forget_direct(&self, home: &Path, mac: &str) -> Result<Vec<String>>;
    /// Whether a failure of this transport, found among an error's causes, left the machine.
    fn delivery(&self, cause: &(dyn std::error::Error + 'static)) -> Option<Delivery>;
}

static TRANSPORT: OnceLock<Arc<dyn Transport>> = OnceLock::new();

/// Installs the process's transport; the first one stays.
pub fn install(transport: Arc<dyn Transport>) {
    let _ = TRANSPORT.set(transport);
}

pub fn installed() -> Option<&'static Arc<dyn Transport>> {
    #[cfg(test)]
    install(Arc::new(crate::test_transport::Fake));
    TRANSPORT.get()
}

/// The installed transport; before the root installs one, every request fails as never sent.
pub fn transport() -> &'static Arc<dyn Transport> {
    static UNWIRED: std::sync::LazyLock<Arc<dyn Transport>> = std::sync::LazyLock::new(|| Arc::new(Unwired));
    installed().unwrap_or(&UNWIRED)
}

/// What a process that installed no transport reaches: nothing.
struct Unwired;

impl Unwired {
    fn unsent<T>() -> Result<T> {
        Err(anyhow::Error::new(crate::client::Unsent("this process installed no client transport".into())))
    }
}

impl Local for Unwired {
    fn send(&self, _: Request<'_>, _: Option<&ResponseObserver>) -> Result<(u16, Vec<u8>)> {
        Self::unsent()
    }

    fn domain_get(&self, _: &str, _: &str) -> Result<(u16, Vec<u8>)> {
        Self::unsent()
    }

    fn domain_call(&self, _: Request<'_>, _: &str) -> Result<(u16, Option<String>, Vec<u8>)> {
        Self::unsent()
    }

    fn bounded(&self, _: Duration) -> Arc<dyn Local> {
        Arc::new(Unwired)
    }
}

impl Transport for Unwired {
    fn local(&self, _: Duration, _: bool) -> Arc<dyn Local> {
        Arc::new(Unwired)
    }

    fn at(&self, _: String) -> Arc<dyn Local> {
        Arc::new(Unwired)
    }

    fn relay(&self) -> Option<Arc<dyn Remote>> {
        None
    }

    fn relay_target(&self) -> Option<&'static str> {
        None
    }

    fn select_relay(&self, _: Option<&str>) -> Result<()> {
        Self::unsent()
    }

    fn relay_paired(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn relay_machines(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn connect_relay(&self, _: &str, _: bool) -> Result<()> {
        Self::unsent()
    }

    fn forget_relay(&self, _: &str) -> Result<bool> {
        Ok(false)
    }

    fn direct_macs(&self) -> Result<Vec<Arc<dyn DirectMac>>> {
        Ok(Vec::new())
    }

    fn connect_direct(&self, _: Option<&str>, _: bool) -> Result<()> {
        Self::unsent()
    }

    fn forget_direct(&self, _: &Path, _: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn delivery(&self, _: &(dyn std::error::Error + 'static)) -> Option<Delivery> {
        None
    }
}
