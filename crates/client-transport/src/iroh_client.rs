//! A paired Mac over its iroh endpoint (`docs/iroh-protocol.md`): one QUIC connection, directly
//! or through the iroh relay, opened with a hello signed by this terminal's pairing key, then one
//! sealed envelope per stream. This terminal has one iroh key for every Mac, kept beside its
//! pairings; what it needs to dial each Mac comes from that Mac's encrypted `/routes`.
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use iroh::endpoint::{Connection, PortmapperConfig, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMode, RelayUrl, SecretKey};
use p256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const ALPN: &[u8] = b"alondra/1";
const KEY_FILE: &str = "iroh-client.key";
const STORE: &str = "direct-iroh.json";
const MAX_REPLY: usize = 8 * 1024 * 1024;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// What dials one Mac's endpoint, as its `/routes` gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dial {
    pub endpoint: String,
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub relay: Option<String>,
}

impl Dial {
    /// The dial `/routes` describes, when that Mac runs an iroh endpoint.
    pub fn from_routes(body: &Value) -> Option<Self> {
        let endpoint = body["iroh_endpoint"].as_str()?.parse::<EndpointId>().ok()?.to_string();
        let addrs = body["iroh_addrs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|addr| addr.parse::<SocketAddr>().is_ok())
            .map(str::to_owned)
            .collect();
        let relay = body["iroh_relay"].as_str().filter(|url| url.parse::<RelayUrl>().is_ok()).map(str::to_owned);
        Some(Self { endpoint, addrs, relay })
    }
}

fn store_path(home: &Path) -> std::path::PathBuf {
    home.join(STORE)
}

pub fn load_dial(home: &Path, mac_id: &str) -> Option<Dial> {
    let bytes = fs::read(store_path(home)).ok()?;
    let mut dials: BTreeMap<String, Dial> = serde_json::from_slice(&bytes).ok()?;
    dials.remove(mac_id)
}

/// Keeps `dial` for `mac_id`, or forgets it with `None`. Callers hold the pairing store's lock.
pub fn save_dial(home: &Path, mac_id: &str, dial: Option<Dial>) -> Result<()> {
    let path = store_path(home);
    let mut dials: BTreeMap<String, Dial> =
        fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    match dial {
        Some(dial) => dials.insert(mac_id.to_owned(), dial),
        None => dials.remove(mac_id),
    };
    write_private(&path, &serde_json::to_vec_pretty(&dials)?)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// This terminal's iroh key, created on first use; an unreadable or shared file is refused.
pub fn key(home: &Path) -> Result<SecretKey> {
    let path = home.join(KEY_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                bail!("{} must be a regular owner-only file", path.display());
            }
            let bytes: [u8; 32] = B64
                .decode(fs::read_to_string(&path)?.trim())?
                .try_into()
                .map_err(|_| anyhow!("{} is not an iroh key", path.display()))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(home)?;
            let mut bytes = [0u8; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
                .map_err(|_| anyhow!("OS randomness failed"))?;
            let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            file.write_all(B64.encode(bytes).as_bytes())?;
            file.sync_all()?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) => Err(error.into()),
    }
}

/// The process's one runtime, which drives every endpoint it binds.
static RUNTIME: OnceLock<std::result::Result<tokio::runtime::Runtime, String>> = OnceLock::new();

fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("alondra-iroh")
                .enable_all()
                .build()
                .map_err(|error| format!("{error:#}"))
        })
        .as_ref()
        .map_err(|error| anyhow!("iroh endpoint unavailable: {error}"))
}

/// The iroh endpoint a home dials from. A newer one replaces it after the Mac slept or when a
/// connection stays on the relay; it closes once no session uses it.
struct Local {
    runtime: &'static tokio::runtime::Runtime,
    endpoint: Endpoint,
    home: PathBuf,
    generation: u64,
}

impl Drop for Local {
    fn drop(&mut self) {
        let endpoint = self.endpoint.clone();
        self.runtime.spawn(async move { endpoint.close().await });
    }
}

static LOCALS: Mutex<BTreeMap<PathBuf, Arc<Local>>> = Mutex::new(BTreeMap::new());
static GENERATIONS: AtomicU64 = AtomicU64::new(0);

fn locals() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Arc<Local>>> {
    LOCALS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn local(home: &Path) -> Result<Arc<Local>> {
    let mut locals = locals();
    if let Some(local) = locals.get(home) {
        return Ok(local.clone());
    }
    let local = bind(home)?;
    locals.insert(home.to_path_buf(), local.clone());
    Ok(local)
}

fn bind(home: &Path) -> Result<Arc<Local>> {
    let runtime = runtime()?;
    let key = key(home)?;
    let endpoint = runtime
        .block_on(
            Endpoint::builder(presets::Minimal)
                .secret_key(key)
                .relay_mode(RelayMode::Custom(iroh::RelayMap::empty()))
                .portmapper_config(PortmapperConfig::Disabled)
                .bind(),
        )
        .map_err(|error| anyhow!("iroh endpoint unavailable: {error:#}"))?;
    let generation = GENERATIONS.fetch_add(1, Ordering::SeqCst) + 1;
    Ok(Arc::new(Local { runtime, endpoint, home: home.to_path_buf(), generation }))
}

/// A wall clock reading beside a monotonic one. On macOS the monotonic clock stands still while
/// the Mac sleeps, so the wall clock running ahead of it tells that the Mac slept.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clocks {
    pub(crate) wall: SystemTime,
    pub(crate) monotonic: Instant,
}

impl Clocks {
    fn now() -> Self {
        Self { wall: SystemTime::now(), monotonic: Instant::now() }
    }
}

/// How far the wall clock must run ahead of the monotonic one to count as a sleep.
pub(crate) const SLEPT: Duration = Duration::from_secs(30);

pub(crate) fn slept(before: Clocks, now: Clocks) -> bool {
    let wall = now.wall.duration_since(before.wall).unwrap_or_default();
    wall.saturating_sub(now.monotonic.saturating_duration_since(before.monotonic)) > SLEPT
}

static AWAKE: Mutex<BTreeMap<PathBuf, Clocks>> = Mutex::new(BTreeMap::new());

fn awake() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Clocks>> {
    AWAKE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// True once after the Mac slept since `home` last asked; its endpoint is then retired, so the
/// next connection holepunches on the network the Mac woke up on.
pub fn woke(home: &Path) -> bool {
    let now = Clocks::now();
    let before = awake().insert(home.to_path_buf(), now);
    let slept = before.is_some_and(|before| slept(before, now));
    if slept {
        locals().remove(home);
    }
    slept
}

/// Makes `home` read as if the Mac had slept for `asleep` since it last asked.
#[cfg(test)]
pub(crate) fn pretend_slept(home: &Path, asleep: Duration) {
    let mut awake = awake();
    let clocks = awake.entry(home.to_path_buf()).or_insert_with(Clocks::now);
    clocks.wall -= asleep;
}

/// The generation of the endpoint `home` dials from now, if it has one.
#[cfg(test)]
pub(crate) fn generation(home: &Path) -> Option<u64> {
    locals().get(home).map(|local| local.generation)
}

/// How many connections `home` started dialing.
#[cfg(test)]
pub(crate) fn dials(home: &Path) -> usize {
    DIALS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(home).copied().unwrap_or(0)
}

#[cfg(test)]
static DIALS: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());

/// How long iroh waits after each failure in a row before it dials the Mac again.
pub(crate) const BACKOFF: [Duration; 4] =
    [Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(30), Duration::from_secs(60)];

/// When a link to a Mac may dial again: after each failure in a row it waits longer, and an
/// answer or a sleep starts it over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Backoff {
    failures: usize,
    until: Option<Instant>,
}

impl Backoff {
    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.until.is_none_or(|until| now >= until)
    }

    pub(crate) fn failed(&mut self, now: Instant) {
        self.hold(now, BACKOFF[self.failures.min(BACKOFF.len() - 1)]);
        self.failures += 1;
    }

    /// Waits at least `wait`, without counting a failure.
    pub(crate) fn hold(&mut self, now: Instant, wait: Duration) {
        self.until = Some(self.until.map_or(now + wait, |until| until.max(now + wait)));
    }

    pub(crate) fn answered(&mut self) {
        *self = Self::default();
    }
}

/// How long a connection may go only through the relay before a new one tries to go direct.
pub(crate) const RELAY_STUCK: Duration = Duration::from_secs(2 * 60);
/// The least time between two of those tries.
pub(crate) const RELAY_RETRY: Duration = Duration::from_secs(10 * 60);

/// Follows whether a connection keeps going through the relay, and says when to try a new one.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RelayWatch {
    since: Option<Instant>,
    tried: Option<Instant>,
}

impl RelayWatch {
    /// The connection's path seen at `now`; true when a new connection should try to go direct.
    pub(crate) fn seen(&mut self, path: Option<PathKind>, now: Instant) -> bool {
        if !matches!(path, Some(PathKind::Relay)) {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        let stuck = now.saturating_duration_since(since) >= RELAY_STUCK;
        if stuck && self.tried.is_none_or(|tried| now.saturating_duration_since(tried) >= RELAY_RETRY) {
            self.since = None;
            self.tried = Some(now);
            return true;
        }
        false
    }
}

/// How the connection reaches the Mac right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    Direct(SocketAddr),
    Relay,
}

/// What the Mac answered on one stream.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// A sealed response envelope: `response`, or `revoked` (which opens to `device_revoked`).
    Sealed(Vec<u8>),
    Refused { error: String, detail: Option<String> },
}

/// Whether a failed exchange may have reached the Mac.
#[derive(Debug)]
pub struct Failed {
    pub sent: bool,
}

/// One open connection to one Mac, after its hello was accepted.
pub struct Session {
    local: Arc<Local>,
    connection: Connection,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"done");
    }
}

pub struct Identity<'a> {
    pub mac_id: &'a str,
    pub device_id: &'a str,
    pub signing: &'a SigningKey,
}

/// Connects to the Mac `dial` names and says hello as `identity`, within `CONNECT_TIMEOUT`.
pub fn connect(home: &Path, dial: &Dial, identity: &Identity) -> Result<Session> {
    connect_from(local(home)?, dial, identity)
}

/// Like `connect`, from a new endpoint that becomes the one `home` dials from once the Mac
/// accepts it; until then the current one and its sessions go on.
pub fn connect_afresh(home: &Path, dial: &Dial, identity: &Identity) -> Result<Session> {
    let session = connect_from(bind(home)?, dial, identity)?;
    locals().insert(home.to_path_buf(), session.local.clone());
    Ok(session)
}

fn connect_from(local: Arc<Local>, dial: &Dial, identity: &Identity) -> Result<Session> {
    #[cfg(test)]
    {
        *DIALS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(local.home.clone()).or_default() += 1;
    }
    let mac: EndpointId = dial.endpoint.parse().context("invalid Mac iroh endpoint")?;
    let mut addr = EndpointAddr::new(mac);
    for ip in dial.addrs.iter().filter_map(|addr| addr.parse::<SocketAddr>().ok()) {
        addr = addr.with_ip_addr(ip);
    }
    let relay = dial.relay.as_deref().map(str::parse::<RelayUrl>).transpose().context("invalid iroh relay")?;
    if let Some(relay) = &relay {
        addr = addr.with_relay_url(relay.clone());
    }
    let endpoint = local.endpoint.clone();
    let signature = alondra_e2e::sign_iroh_hello(
        identity.signing,
        identity.mac_id,
        identity.device_id,
        endpoint.id().as_bytes(),
        mac.as_bytes(),
    )
    .map_err(|error| anyhow!("sign the iroh hello: {error:?}"))?;
    let hello = serde_json::to_vec(&serde_json::json!({"device_id": identity.device_id, "signature": signature}))?;
    let connection = local.runtime.block_on(async {
        if let Some(relay) = relay {
            endpoint.insert_relay(relay.clone(), Arc::new(RelayConfig::from(relay))).await;
        }
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            let connection = endpoint.connect(addr, ALPN).await?;
            let reply: Value = serde_json::from_slice(&exchange(&connection, &hello).await?)?;
            if reply != serde_json::json!({"ok": true}) {
                bail!("the Mac refused this terminal's iroh hello: {reply}");
            }
            anyhow::Ok(connection)
        })
        .await
        .map_err(|_| anyhow!("no iroh connection within {} s", CONNECT_TIMEOUT.as_secs()))?
    })?;
    Ok(Session { local, connection })
}

async fn exchange(connection: &Connection, body: &[u8]) -> Result<Vec<u8>> {
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(body).await?;
    send.finish()?;
    Ok(recv.read_to_end(MAX_REPLY).await?)
}

impl Session {
    /// Sends one sealed request envelope on its own stream and waits up to `timeout` for the answer.
    pub fn send(&self, envelope: &[u8], timeout: Duration) -> std::result::Result<Reply, Failed> {
        self.local.runtime.block_on(async {
            let (mut send, mut recv) = self
                .connection
                .open_bi()
                .await
                .map_err(|_| Failed { sent: false })?;
            fn lost<E>(_: E) -> Failed {
                Failed { sent: true }
            }
            send.write_all(envelope).await.map_err(lost)?;
            send.finish().map_err(lost)?;
            let bytes = tokio::time::timeout(timeout, recv.read_to_end(MAX_REPLY)).await.map_err(lost)?.map_err(lost)?;
            reply(&bytes).map_err(lost)
        })
    }

    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }

    /// Open, and dialed from the endpoint its home dials from now.
    pub fn is_usable(&self) -> bool {
        let current = locals().get(&self.local.home).is_some_and(|local| local.generation == self.local.generation);
        current && !self.is_closed()
    }

    /// The path iroh uses for this connection now.
    pub fn path(&self) -> Option<PathKind> {
        let paths = self.connection.paths();
        let selected = paths.iter().find(|path| path.is_selected())?;
        match selected.remote_addr() {
            iroh::TransportAddr::Ip(addr) => Some(PathKind::Direct(*addr)),
            _ => Some(PathKind::Relay),
        }
    }
}

/// Reads the Mac's answer: a sealed envelope to open, or why the Mac refused the request.
pub fn reply(bytes: &[u8]) -> Result<Reply> {
    let mut answer: Value = serde_json::from_slice(bytes).context("the Mac's iroh answer is not JSON")?;
    let sealed = answer.get_mut("response").map(Value::take);
    let sealed = sealed.or_else(|| answer.get_mut("revoked").map(Value::take));
    match (sealed, answer["error"].as_str()) {
        (Some(sealed), None) if sealed.is_object() => Ok(Reply::Sealed(serde_json::to_vec(&sealed)?)),
        (None, Some(error)) => {
            Ok(Reply::Refused { error: error.to_owned(), detail: answer["detail"].as_str().map(str::to_owned) })
        }
        _ => bail!("the Mac's iroh answer has an unknown shape"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reply_keeps_the_sealed_bytes_or_names_the_refusal() {
        assert_eq!(reply(br#"{"response":{"kind":"response","x":[1, 2]}}"#).unwrap(), Reply::Sealed(br#"{"kind":"response","x":[1,2]}"#.to_vec()));
        assert_eq!(reply(br#"{"revoked":{"kind":"response"}}"#).unwrap(), Reply::Sealed(br#"{"kind":"response"}"#.to_vec()));
        assert_eq!(
            reply(br#"{"error":"rejected","detail":"no"}"#).unwrap(),
            Reply::Refused { error: "rejected".into(), detail: Some("no".into()) }
        );
        assert!(reply(br#"{"something":"else"}"#).is_err());
        assert!(reply(b"not json").is_err());
    }

    #[test]
    fn routes_without_a_valid_endpoint_give_no_dial_and_bad_addresses_are_dropped() {
        let endpoint = SecretKey::from_bytes(&[3u8; 32]).public().to_string();
        assert_eq!(Dial::from_routes(&json!({"lan": ["192.168.1.2"]})), None);
        assert_eq!(Dial::from_routes(&json!({"iroh_endpoint": "nope"})), None);
        let body = json!({"iroh_endpoint": endpoint, "iroh_addrs": ["192.168.1.2:5000", "garbage", "[fd12::1]:5000"],
            "iroh_relay": "https://iroh.example.com/"});
        assert_eq!(
            Dial::from_routes(&body),
            Some(Dial {
                endpoint,
                addrs: vec!["192.168.1.2:5000".into(), "[fd12::1]:5000".into()],
                relay: Some("https://iroh.example.com/".into())
            })
        );
    }

    #[test]
    fn the_key_is_created_owner_only_once_and_a_readable_one_is_refused() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&*home).unwrap();
        let first = key(&home).unwrap();
        assert_eq!(key(&home).unwrap().public(), first.public());
        let path = home.join(KEY_FILE);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(key(&home).is_err());
    }

    #[test]
    fn the_mac_slept_when_the_wall_clock_runs_ahead_of_the_monotonic_one() {
        let before = Clocks { wall: SystemTime::now(), monotonic: Instant::now() };
        let after = |wall: u64, monotonic: u64| Clocks {
            wall: before.wall + Duration::from_secs(wall),
            monotonic: before.monotonic + Duration::from_secs(monotonic),
        };
        assert!(slept(before, after(600, 1)), "ten minutes asleep");
        assert!(slept(before, after(45, 5)), "forty seconds asleep");
        assert!(!slept(before, after(20, 20)), "awake all along");
        assert!(!slept(before, after(25, 0)), "under the threshold");
        let back = Clocks { wall: before.wall - Duration::from_secs(600), monotonic: before.monotonic };
        assert!(!slept(before, back), "a wall clock set back is no sleep");
    }

    #[test]
    fn failures_in_a_row_wait_longer_until_an_answer_starts_over() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut backoff = Backoff::default();
        assert!(backoff.ready(start));
        let mut now = start;
        for wait in [5, 15, 30, 60, 60] {
            backoff.failed(now);
            assert!(!backoff.ready(now + Duration::from_secs(wait - 1)), "waits {wait} s");
            now += Duration::from_secs(wait);
            assert!(backoff.ready(now), "ready after {wait} s");
        }
        backoff.answered();
        backoff.failed(now);
        assert!(backoff.ready(now + Duration::from_secs(5)), "an answer starts the waits over");
        let mut held = Backoff::default();
        held.hold(start, Duration::from_secs(60));
        held.failed(start);
        assert!(!held.ready(at(59)), "a failure never shortens a longer hold");
        assert!(held.ready(at(60)));
    }

    #[test]
    fn a_connection_only_on_the_relay_for_two_minutes_tries_a_new_one_at_most_every_ten() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let relay = Some(PathKind::Relay);
        let direct = Some(PathKind::Direct("192.0.2.1:7".parse().unwrap()));
        let mut watch = RelayWatch::default();
        assert!(!watch.seen(relay, at(0)));
        assert!(!watch.seen(relay, at(119)));
        assert!(watch.seen(relay, at(120)), "two minutes on the relay");
        assert!(!watch.seen(relay, at(121)));
        assert!(!watch.seen(relay, at(300)), "stuck again, but the last try was five minutes ago");
        assert!(watch.seen(relay, at(720)), "ten minutes after the last try");
        let mut watch = RelayWatch::default();
        assert!(!watch.seen(relay, at(0)));
        assert!(!watch.seen(direct, at(100)), "a direct path is never stuck");
        assert!(!watch.seen(relay, at(130)), "the relay time starts over after a direct path");
        assert!(!watch.seen(None, at(200)));
        assert!(!watch.seen(relay, at(260)));
        assert!(watch.seen(relay, at(380)));
    }

    #[test]
    fn a_dial_is_kept_per_mac_and_can_be_forgotten() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&*home).unwrap();
        let dial = |n: u8| Dial { endpoint: SecretKey::from_bytes(&[n; 32]).public().to_string(), addrs: vec![], relay: None };
        save_dial(&home, "mac-a", Some(dial(1))).unwrap();
        save_dial(&home, "mac-b", Some(dial(2))).unwrap();
        save_dial(&home, "mac-a", None).unwrap();
        assert_eq!(load_dial(&home, "mac-a"), None);
        assert_eq!(load_dial(&home, "mac-b"), Some(dial(2)));
        assert_eq!(fs::metadata(store_path(&home)).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
