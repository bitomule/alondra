//! The iroh endpoint: a paired device keeps one QUIC connection to this Mac, through the iroh
//! relay or directly, and sends each encrypted envelope on its own stream. The connection serves
//! nothing until its first stream proves, with the device key paired here, that the connecting
//! iroh endpoint speaks for that device. Every envelope then goes through
//! `Service::handle_direct`, so it is checked, journaled and answered exactly as on the direct
//! listener. The wire protocol is in `docs/iroh-protocol.md`.
use crate::locks::Poisonable as _;
use crate::service::{DirectRefusal, Service};
use alondra_e2e::Envelope;
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::endpoint::{Connection, PortmapperConfig, QuicTransportConfig, RecvStream, SendStream, VarInt, presets};
use iroh::{Endpoint, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) const ALPN: &[u8] = b"alondra/1";
pub(crate) const KEY_FILE: &str = "iroh.key";
/// The UDP port tried first, so a port forwarded on the router keeps working after a restart.
pub(crate) const DEFAULT_PORT: u16 = 47811;
#[cfg(not(test))]
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const HELLO_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_HELLO: usize = 4 * 1024;
pub(crate) const MAX_REQUEST: usize = 256 * 1024;
pub(crate) const MAX_STREAMS: u32 = 32;
pub(crate) const MAX_CONNECTIONS: usize = 64;
const RETRY: Duration = Duration::from_secs(10);

/// Application close codes a client reads when the Mac ends its connection.
pub(crate) mod close {
    pub(crate) const UNAUTHORIZED: u32 = 1;
    pub(crate) const HELLO_TIMEOUT: u32 = 2;
    pub(crate) const BUSY: u32 = 3;
}

/// The iroh endpoint in `service.toml`'s `[iroh]` table.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct IrohConfig {
    pub(crate) enabled: bool,
    /// `auto`: the iroh relay the Alondra relay announces; `off`: none; or that relay's URL.
    pub(crate) relay: String,
    /// The UDP port, or 0 for any free one. The default falls back to a free one when taken.
    pub(crate) port: u16,
    /// Binds only this address instead of every interface.
    pub(crate) listen: Option<IpAddr>,
    /// Asks the router for a port mapping (UPnP, NAT-PMP, PCP).
    pub(crate) portmapper: bool,
}

impl Default for IrohConfig {
    fn default() -> Self {
        Self { enabled: true, relay: "auto".into(), port: DEFAULT_PORT, listen: None, portmapper: true }
    }
}

impl IrohConfig {
    pub(crate) fn fixed_relay(&self) -> Result<Option<RelayUrl>> {
        match self.relay.as_str() {
            "auto" | "off" => Ok(None),
            url => Ok(Some(relay_url(url)?)),
        }
    }
}

pub(crate) fn relay_url(url: &str) -> Result<RelayUrl> {
    let parsed: RelayUrl = url.parse().with_context(|| format!("invalid iroh relay URL {url}"))?;
    let loopback = parsed.scheme() == "http" && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"));
    anyhow::ensure!(parsed.scheme() == "https" || loopback, "the iroh relay must use https");
    Ok(parsed)
}

/// This Mac's iroh identity, beside its other service keys. The first daemon to start writes it;
/// a concurrent one reads the same key.
pub(crate) fn load_or_create_key(dir: &Path) -> Result<SecretKey> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = dir.join(KEY_FILE);
    if !path.exists() {
        let mut bytes = [0u8; 32];
        SystemRandom::new().fill(&mut bytes).map_err(|_| anyhow!("OS randomness failed"))?;
        let temporary = dir.join(format!(".{KEY_FILE}.{}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(URL_SAFE_NO_PAD.encode(bytes).as_bytes())?;
        file.sync_all()?;
        let linked = std::fs::hard_link(&temporary, &path);
        std::fs::remove_file(&temporary)?;
        match linked {
            Ok(()) => return Ok(SecretKey::from_bytes(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).with_context(|| format!("write {}", path.display())),
        }
    }
    Ok(SecretKey::from_bytes(&crate::service::private_key(&path)?))
}

#[derive(Default)]
struct Status {
    endpoint: Option<String>,
    relay: Option<String>,
    relay_connected: bool,
    relay_refusal: Option<String>,
}

/// A holder that panics leaves it whole, so it is recovered: plain fields set one by one.
static STATUS: Mutex<Option<Status>> = Mutex::new(None);

fn update(change: impl FnOnce(&mut Status)) {
    change(STATUS.lock_or_recover("the iroh status").get_or_insert_with(Status::default));
}

/// What `/daemon` reports about the endpoint and macOS's Local Network permission.
pub(crate) fn status() -> serde_json::Value {
    let reachability = crate::reachability::current();
    let guard = STATUS.lock_or_recover("the iroh status");
    let local_network = crate::local_network::current();
    match &*guard {
        None => json!({"endpoint": null, "local_network": local_network, "reachability": reachability}),
        Some(status) => json!({
            "endpoint": status.endpoint, "relay": status.relay, "relay_connected": status.relay_connected,
            "relay_refusal": status.relay_refusal, "local_network": local_network,
            "reachability": reachability,
        }),
    }
}

/// The iroh relay in use and whether this Mac is connected to it.
pub(crate) fn relay_state() -> (Option<String>, bool) {
    match &*STATUS.lock_or_recover("the iroh status") {
        Some(status) => (status.relay.clone(), status.relay_connected),
        None => (None, false),
    }
}

pub(crate) async fn bind(key: SecretKey, config: &IrohConfig) -> Result<Endpoint> {
    let transport = QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_STREAMS))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .build();
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Custom(RelayMap::empty()))
        .transport_config(transport)
        .portmapper_config(if config.portmapper { PortmapperConfig::default() } else { PortmapperConfig::Disabled });
    builder = match config.listen {
        Some(ip) => builder.clear_ip_transports().bind_addr(SocketAddr::new(ip, config.port))?,
        None if config.port != 0 => builder
            .bind_addr(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), config.port))?
            .bind_addr(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), config.port))?,
        None => builder,
    };
    Ok(builder.bind().await?)
}

/// Serves the endpoint for as long as the daemon runs, binding again after a failure.
pub(crate) async fn run(service: Arc<Service>) {
    let config = service.iroh_config().clone();
    let (endpoint, port_fixed) = loop {
        if let Some(bound) = bind_preferring_port(service.iroh_key(), &config).await {
            break bound;
        }
        tokio::time::sleep(RETRY).await;
    };
    log!("iroh endpoint {} on {:?}", endpoint.id(), endpoint.bound_sockets());
    update(|status| status.endpoint = Some(endpoint.id().to_string()));
    let portmapper = if config.portmapper { PortmapperConfig::default() } else { PortmapperConfig::Disabled };
    tokio::spawn(follow_relay(endpoint.clone(), service.clone(), config));
    tokio::spawn(report_relay(endpoint.clone()));
    tokio::spawn(publish_addresses(endpoint.clone(), service.clone()));
    let watched = endpoint.clone();
    tokio::spawn(async move { crate::reachability::watch(watched, port_fixed, &portmapper).await });
    serve(endpoint, service).await;
}

/// Binds the configured port; when the default one is taken, any free one. The flag says whether
/// the port is the same on every start.
pub(crate) async fn bind_preferring_port(key: &SecretKey, config: &IrohConfig) -> Option<(Endpoint, bool)> {
    match bind(key.clone(), config).await {
        Ok(endpoint) => return Some((endpoint, config.port != 0)),
        Err(error) if config.port == DEFAULT_PORT => log!("iroh port {DEFAULT_PORT} not free, using any: {error:#}"),
        Err(error) => {
            log!("iroh endpoint cannot bind: {error:#}");
            return None;
        }
    }
    match bind(key.clone(), &IrohConfig { port: 0, ..config.clone() }).await {
        Ok(endpoint) => Some((endpoint, false)),
        Err(error) => {
            log!("iroh endpoint cannot bind: {error:#}");
            None
        }
    }
}

/// Accepts connections until the endpoint closes.
pub(crate) async fn serve(endpoint: Endpoint, service: Arc<Service>) {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    while let Some(incoming) = endpoint.accept().await {
        let permit = permits.clone().try_acquire_owned();
        let service = service.clone();
        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(error) => return log!("iroh connection failed: {error}"),
            };
            let Ok(_permit) = permit else {
                return connection.close(VarInt::from_u32(close::BUSY), b"busy");
            };
            serve_connection(&service, connection).await;
        });
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    device_id: String,
    signature: String,
}

async fn serve_connection(service: &Arc<Service>, connection: Connection) {
    let device_id = match tokio::time::timeout(HELLO_TIMEOUT, hello(service, &connection)).await {
        Ok(Some(device_id)) => device_id,
        Ok(None) => return connection.close(VarInt::from_u32(close::UNAUTHORIZED), b"unauthorized"),
        Err(_) => return connection.close(VarInt::from_u32(close::HELLO_TIMEOUT), b"no hello"),
    };
    let opened = std::time::Instant::now();
    let followed = tokio::spawn(crate::reachability::follow(connection.clone()));
    while let Ok((send, recv)) = connection.accept_bi().await {
        let (service, device_id) = (service.clone(), device_id.clone());
        tokio::spawn(async move { serve_stream(&service, &device_id, send, recv).await });
    }
    if let Ok(crate::reachability::Followed { best: Some(kind), bytes }) = followed.await {
        crate::reachability::record_session(kind, bytes);
        log!(
            "iroh connection from {device_id} closed after {:?}, path {kind:?}, bytes lan={} internet={} relay={}",
            opened.elapsed(),
            bytes.lan,
            bytes.internet,
            bytes.relay
        );
    }
}

/// The first stream: the device ID and its signature over both endpoints. `None` refuses it.
async fn hello(service: &Service, connection: &Connection) -> Option<String> {
    let (mut send, mut recv) = connection.accept_bi().await.ok()?;
    let hello: Option<Hello> =
        recv.read_to_end(MAX_HELLO).await.ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let proven = hello.filter(|hello| {
        service.iroh_hello_proven(&hello.device_id, &hello.signature, connection.remote_id().as_bytes())
    });
    if let Some(hello) = &proven {
        service.remember_iroh_hello(&hello.device_id, &connection.remote_id().to_string(), &hello.signature);
    }
    let reply = if proven.is_some() { json!({"ok": true}) } else { json!({"error": "unauthorized"}) };
    if send.write_all(reply.to_string().as_bytes()).await.is_ok() {
        let _ = send.finish();
        let _ = send.stopped().await;
    }
    proven.map(|hello| hello.device_id)
}

async fn serve_stream(service: &Service, device_id: &str, mut send: SendStream, mut recv: RecvStream) {
    let reply = match recv.read_to_end(MAX_REQUEST).await {
        Ok(bytes) => answer(service, device_id, &bytes).await,
        Err(iroh::endpoint::ReadToEndError::TooLong) => refusal("too_large", None),
        Err(_) => return,
    };
    if send.write_all(&reply).await.is_ok() {
        let _ = send.finish();
    }
}

pub(crate) async fn answer(service: &Service, device_id: &str, body: &[u8]) -> Vec<u8> {
    let Ok(envelope) = serde_json::from_slice::<Envelope>(body) else {
        return refusal("invalid_envelope", None);
    };
    if envelope.sender != device_id {
        return refusal("wrong_sender", None);
    }
    match service.handle_direct(envelope).await {
        Ok((sealed, lease)) => {
            drop(lease);
            wrapped("response", &sealed)
        }
        Err(DirectRefusal::Revoked(sealed)) => wrapped("revoked", &sealed),
        Err(DirectRefusal::Misdirected) => refusal("wrong_mac", None),
        Err(DirectRefusal::Unauthorized) => refusal("unauthorized", None),
        Err(DirectRefusal::Conflict) => refusal("idempotency_conflict", None),
        Err(DirectRefusal::Rejected(detail)) => refusal("rejected", Some(detail)),
    }
}

fn wrapped(key: &str, sealed: &[u8]) -> Vec<u8> {
    [format!("{{\"{key}\":").as_bytes(), sealed, b"}"].concat()
}

fn refusal(code: &str, detail: Option<String>) -> Vec<u8> {
    let mut body = json!({"error": code});
    if let Some(detail) = detail {
        body["detail"] = detail.into();
    }
    body.to_string().into_bytes()
}

/// Keeps the endpoint on the configured iroh relay, or on the one the Alondra relay announces.
async fn follow_relay(endpoint: Endpoint, service: Arc<Service>, config: IrohConfig) {
    let fixed = match config.fixed_relay() {
        Ok(fixed) => fixed,
        Err(error) => return log!("iroh relay not used: {error:#}"),
    };
    if config.relay == "off" {
        return;
    }
    if let Some(url) = fixed {
        return use_relay(&endpoint, &service, None, url).await;
    }
    let mut announced = service.iroh_relay_announced();
    let mut current: Option<RelayUrl> = None;
    loop {
        let next = announced.borrow_and_update().clone();
        match next.as_deref().map(relay_url) {
            Some(Ok(url)) if current.as_ref() != Some(&url) => {
                use_relay(&endpoint, &service, current.take(), url.clone()).await;
                current = Some(url);
            }
            Some(Err(error)) => log!("announced iroh relay not used: {error:#}"),
            _ => {}
        }
        if announced.changed().await.is_err() {
            return;
        }
    }
}

async fn use_relay(endpoint: &Endpoint, service: &Service, previous: Option<RelayUrl>, url: RelayUrl) {
    if let Some(previous) = previous {
        endpoint.remove_relay(&previous).await;
    }
    log!("iroh endpoint uses relay {url}");
    update(|status| status.relay = Some(url.to_string()));
    service.set_iroh_dial(|dial| dial.relay = Some(url.to_string()));
    endpoint.insert_relay(url.clone(), Arc::new(RelayConfig::from(url))).await;
}

/// Keeps the direct addresses devices are told to dial this endpoint at up to date.
async fn publish_addresses(endpoint: Endpoint, service: Arc<Service>) {
    let mut watcher = endpoint.watch_addr();
    loop {
        let addresses = watcher.get().ip_addrs().map(ToString::to_string).collect();
        service.set_iroh_dial(|dial| dial.addresses = addresses);
        if watcher.updated().await.is_err() {
            return;
        }
    }
}

/// Logs when the relay connection comes up, drops, or refuses this Mac (not yet bound).
async fn report_relay(endpoint: Endpoint) {
    let mut watcher = endpoint.home_relay_status();
    let mut last = None;
    loop {
        let statuses = watcher.get();
        let now = statuses.first().map(|status| (status.is_connected(), status.auth_denied_reason().map(str::to_owned)));
        if now != last {
            let (connected, refusal) = now.clone().unwrap_or((false, None));
            match &refusal {
                Some(reason) => log!("iroh relay refused this Mac: {reason}"),
                None if connected => log!("iroh relay connected"),
                None => {}
            }
            update(|status| {
                status.relay_connected = connected;
                status.relay_refusal = refusal;
            });
            last = now;
        }
        if watcher.updated().await.is_err() {
            return;
        }
    }
}
