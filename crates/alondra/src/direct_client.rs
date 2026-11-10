//! Talking to Macs directly on the local network or a tailnet, without the relay. Every Mac is
//! its own pairing: its pinned keys, this terminal's keys for it, and the addresses it can be
//! reached at. Requests are the same end-to-end envelopes the relay carries; a route is used
//! only after `GET /direct/hello` there returns exactly the pinned Mac.
use alondra_e2e::pairing::{self as protocol, Approval, Transcript};
use alondra_e2e::{Context as EnvelopeContext, Envelope};
use crate::client::{Answered, Unanswered, Unsent};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::{SigningKey, VerifyingKey};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_PORT: u16 = 47810;
const STORE: &str = "direct-macs.json";
const LOCK: &str = "direct-macs.lock";
const HELLO_TIMEOUT: Duration = Duration::from_millis(1500);
const PAIRING_WAIT: Duration = Duration::from_secs(130);
/// After an iroh connection failed, requests go over HTTP this long before iroh is tried again.
const IROH_RETRY: Duration = Duration::from_secs(60);

/// One Mac this terminal paired with directly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectMac {
    pub mac_id: String,
    pub name: String,
    pub mac_signing_key: String,
    pub mac_agreement_key: String,
    pub device_id: String,
    signing_key: String,
    agreement_key: String,
    /// `host:port`, best first: LAN addresses, then the tailnet address, then names.
    pub routes: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    macs: Vec<DirectMac>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub mac_id: String,
    pub name: String,
    pub signing_key: String,
    pub agreement_key: String,
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

pub fn route_kind(route: &str) -> RouteKind {
    let host = route.rsplit_once(':').map_or(route, |(host, _)| host).trim_matches(['[', ']']);
    let tailnet = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]),
        Ok(std::net::IpAddr::V6(v6)) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        Err(_) => host.ends_with(".ts.net"),
    };
    if tailnet { RouteKind::Tailscale } else { RouteKind::Lan }
}

/// The kind of an iroh path: an address on this network or the tailnet keeps its name.
pub fn iroh_route_kind(path: crate::iroh_client::PathKind) -> RouteKind {
    match path {
        crate::iroh_client::PathKind::Relay => RouteKind::Relay,
        crate::iroh_client::PathKind::Direct(addr) => {
            let local = match addr.ip() {
                std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
                std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
            };
            match route_kind(&addr.to_string()) {
                RouteKind::Tailscale => RouteKind::Tailscale,
                _ if local => RouteKind::Lan,
                _ => RouteKind::Direct,
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| anyhow!("OS randomness failed"))?;
    Ok(bytes)
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

fn store_path(home: &Path) -> PathBuf {
    home.join(STORE)
}

struct StoreLock(fs::File);

impl StoreLock {
    fn acquire(home: &Path) -> Result<Self> {
        fs::create_dir_all(home)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home.join(LOCK))?;
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn load_from(home: &Path) -> Result<Vec<DirectMac>> {
    let path = store_path(home);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        bail!("{} must be a regular owner-only file", path.display());
    }
    let store: Store = serde_json::from_slice(&fs::read(&path)?)?;
    Ok(store.macs)
}

pub fn load() -> Result<Vec<DirectMac>> {
    load_from(&crate::client::config().home)
}

fn save_to(home: &Path, macs: Vec<DirectMac>) -> Result<()> {
    let path = store_path(home);
    let bytes = serde_json::to_vec_pretty(&Store { macs })?;
    let temp = path.with_extension(format!("tmp-{}", B64.encode(random::<9>()?)));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temp, &path)?;
    fs::File::open(home)?.sync_all()?;
    Ok(())
}

/// Adds or replaces one Mac's pairing, under the store lock.
pub fn remember(home: &Path, mac: DirectMac) -> Result<()> {
    let _lock = StoreLock::acquire(home)?;
    let mut macs = load_from(home)?;
    macs.retain(|known| known.mac_id != mac.mac_id);
    macs.push(mac);
    save_to(home, macs)
}

/// Removes the Macs named `name` (their id or their name, any case) from this terminal's list.
/// Returns the ids it removed.
pub fn forget(home: &Path, name: &str) -> Result<Vec<String>> {
    let _lock = StoreLock::acquire(home)?;
    let (gone, kept): (Vec<DirectMac>, Vec<DirectMac>) = load_from(home)?.into_iter()
        .partition(|mac| mac.mac_id == name || mac.name.eq_ignore_ascii_case(name));
    if !gone.is_empty() {
        save_to(home, kept)?;
        for mac in &gone {
            crate::iroh_client::save_dial(home, &mac.mac_id, None)?;
        }
    }
    Ok(gone.into_iter().map(|mac| mac.mac_id).collect())
}

/// Keeps, or forgets with `None`, how to dial one Mac over iroh, under the store lock.
pub fn keep_dial(home: &Path, mac_id: &str, dial: Option<crate::iroh_client::Dial>) -> Result<()> {
    let _lock = StoreLock::acquire(home)?;
    crate::iroh_client::save_dial(home, mac_id, dial)
}

fn forget_routes_of_others(home: &Path, mac_id: &str, routes: &[String], dial: Option<crate::iroh_client::Dial>) -> Result<()> {
    let _lock = StoreLock::acquire(home)?;
    let mut macs = load_from(home)?;
    for mac in macs.iter_mut().filter(|mac| mac.mac_id == mac_id) {
        mac.routes = routes.to_vec();
    }
    save_to(home, macs)?;
    crate::iroh_client::save_dial(home, mac_id, dial)
}

fn not_accepted(name: &str) -> anyhow::Error {
    anyhow::Error::new(Answered {
        status: 401,
        message: trf!(
            "{name} no longer accepts this terminal; pair again with: alondra mac connect",
            "{name} ya no acepta este terminal; vuelve a emparejar con: alondra mac connect"
        ),
    })
}

pub fn hello(route: &str) -> Result<Hello> {
    let mut response = agent(HELLO_TIMEOUT).get(format!("http://{route}/direct/hello")).call()?;
    if response.status() != 200 {
        bail!("{route} answered HTTP {}", response.status());
    }
    Ok(serde_json::from_slice(&response.body_mut().with_config().limit(16 * 1024).read_to_vec()?)?)
}

/// What a route's `hello` says about the Mac this terminal pinned.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    Same,
    OtherMac,
    KeyChanged,
}

pub fn check(mac: &DirectMac, hello: &Hello) -> Check {
    if hello.mac_id != mac.mac_id {
        Check::OtherMac
    } else if hello.signing_key != mac.mac_signing_key || hello.agreement_key != mac.mac_agreement_key {
        Check::KeyChanged
    } else {
        Check::Same
    }
}

fn key_changed(name: &str) -> anyhow::Error {
    anyhow!(trf!(
        "The identity of {name} has changed: this terminal no longer trusts it. If you reinstalled that Mac, pair again with: alondra mac connect --repair",
        "La identidad de {name} ha cambiado: este terminal ya no confía en él. Si has reinstalado ese Mac, vuelve a emparejar con: alondra mac connect --repair"
    ))
}

/// The iroh connection to one Mac, when there is one.
enum IrohLink {
    Untried,
    Open(Arc<crate::iroh_client::Session>),
    Down(Instant),
}

/// What became of a request sent over iroh.
enum OverIroh {
    Answered(Result<(u16, Value)>),
    NotSent,
    /// The connection broke after the envelope left: the Mac may have it.
    Lost,
}

/// A live connection to one paired Mac: over iroh when it can, else the HTTP route that last
/// worked.
pub struct Connection {
    mac: DirectMac,
    home: PathBuf,
    current: Mutex<Option<String>>,
    iroh: Mutex<IrohLink>,
    learned: AtomicBool,
}

impl Connection {
    pub fn new(mac: DirectMac) -> Self {
        Self::at(&crate::client::config().home, mac)
    }

    pub fn at(home: &Path, mac: DirectMac) -> Self {
        Self {
            mac,
            home: home.to_path_buf(),
            current: Mutex::new(None),
            iroh: Mutex::new(IrohLink::Untried),
            learned: AtomicBool::new(false),
        }
    }

    pub fn mac(&self) -> &DirectMac {
        &self.mac
    }

    /// The route in use and how it reaches the Mac, once one answered.
    pub fn route(&self) -> Option<(String, RouteKind)> {
        if let IrohLink::Open(session) = &*self.iroh.lock().expect("iroh lock poisoned")
            && let Some(path) = session.path()
        {
            let shown = match path {
                crate::iroh_client::PathKind::Direct(addr) => format!("iroh {addr}"),
                crate::iroh_client::PathKind::Relay => "iroh relay".to_owned(),
            };
            return Some((shown, iroh_route_kind(path)));
        }
        let current = self.current.lock().expect("route lock poisoned").clone()?;
        let kind = route_kind(&current);
        Some((current, kind))
    }

    fn iroh_session(&self) -> Option<Arc<crate::iroh_client::Session>> {
        let mut link = self.iroh.lock().expect("iroh lock poisoned");
        match &*link {
            IrohLink::Open(session) if !session.is_closed() => return Some(session.clone()),
            IrohLink::Down(since) if since.elapsed() < IROH_RETRY => return None,
            _ => {}
        }
        let dial = crate::iroh_client::load_dial(&self.home, &self.mac.mac_id)?;
        let signing = SigningKey::from_slice(&B64.decode(&self.mac.signing_key).ok()?).ok()?;
        let identity = crate::iroh_client::Identity { mac_id: &self.mac.mac_id, device_id: &self.mac.device_id, signing: &signing };
        match crate::iroh_client::connect(&self.home, &dial, &identity) {
            Ok(session) => {
                let session = Arc::new(session);
                *link = IrohLink::Open(session.clone());
                Some(session)
            }
            Err(_) => {
                *link = IrohLink::Down(Instant::now());
                None
            }
        }
    }

    /// Opens the iroh connection now, so the Mac knows this terminal's endpoint and can vouch
    /// for it at the relay. False when the Mac cannot be reached over iroh.
    pub fn greet_over_iroh(&self) -> bool {
        self.iroh_session().is_some()
    }

    fn iroh_down(&self) {
        *self.iroh.lock().expect("iroh lock poisoned") = IrohLink::Down(Instant::now());
    }

    fn over_iroh(&self, bytes: &[u8], request_id: &str, timeout: Duration, observer: Option<&crate::client::availability::ResponseObserver>) -> OverIroh {
        use crate::iroh_client::Reply;
        let Some(session) = self.iroh_session() else { return OverIroh::NotSent };
        let name = &self.mac.name;
        match session.send(bytes, timeout + Duration::from_secs(5)) {
            Ok(Reply::Sealed(sealed)) => {
                if let Some(observer) = observer { observer.received(); }
                OverIroh::Answered(self.open(&sealed, request_id))
            }
            Ok(Reply::Refused { error, detail }) => match error.as_str() {
                "unauthorized" => {
                    if let Some(observer) = observer { observer.received(); }
                    OverIroh::Answered(Err(not_accepted(name)))
                }
                "idempotency_conflict" => {
                    if let Some(observer) = observer { observer.received(); }
                    OverIroh::Answered(Ok((409, json!({"error": "idempotency_conflict"}))))
                }
                "wrong_mac" => {
                    self.iroh_down();
                    OverIroh::NotSent
                }
                _ => {
                    if let Some(observer) = observer { observer.received(); }
                    OverIroh::Answered(Err(anyhow::Error::new(Answered {
                        status: 400,
                        message: detail.unwrap_or(error),
                    })))
                }
            },
            Err(failed) => {
                *self.iroh.lock().expect("iroh lock poisoned") = IrohLink::Untried;
                if failed.sent { OverIroh::Lost } else { OverIroh::NotSent }
            }
        }
    }

    /// Learns, once per connection, how to dial the Mac over iroh when that is not known or no
    /// longer works.
    fn learn_dial_once(&self) {
        let known = crate::iroh_client::load_dial(&self.home, &self.mac.mac_id).is_some();
        let working = matches!(*self.iroh.lock().expect("iroh lock poisoned"), IrohLink::Open(_));
        if (known && working) || self.learned.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.learn_routes(&self.home).is_ok() {
            *self.iroh.lock().expect("iroh lock poisoned") = IrohLink::Untried;
        }
    }

    fn candidates(&self) -> Vec<String> {
        let current = self.current.lock().expect("route lock poisoned").clone();
        let mut routes: Vec<String> = current.into_iter().collect();
        for route in &self.mac.routes {
            if !routes.contains(route) {
                routes.push(route.clone());
            }
        }
        routes
    }

    fn validate(&self, route: &str) -> Result<bool> {
        let Ok(hello) = hello(route) else { return Ok(false) };
        match check(&self.mac, &hello) {
            Check::Same => Ok(true),
            Check::OtherMac => Ok(false),
            Check::KeyChanged => Err(key_changed(&self.mac.name)),
        }
    }

    fn seal(&self, method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<(String, Vec<u8>)> {
        let request_id = format!("op-{}", B64.encode(random::<16>()?));
        let plaintext = serde_json::to_vec(&json!({"method": method, "path": path, "body": body}))?;
        let envelope = Envelope::seal_random(
            EnvelopeContext {
                route: &self.mac.mac_id,
                sender: &self.mac.device_id,
                recipient: &self.mac.mac_id,
                request_id: &request_id,
                kind: "request",
                expires_at_ms: now_ms() + timeout.as_millis().min(590_000) as u64,
            },
            &plaintext,
            &SigningKey::from_slice(&B64.decode(&self.mac.signing_key)?)?,
            &PublicKey::from_sec1_bytes(&B64.decode(&self.mac.mac_agreement_key)?)?,
        )?;
        Ok((request_id, serde_json::to_vec(&envelope)?))
    }

    fn open(&self, bytes: &[u8], request_id: &str) -> Result<(u16, Value)> {
        let response: Envelope = serde_json::from_slice(bytes)?;
        if response.kind != "response" || response.request_id != request_id {
            bail!("response correlation mismatch");
        }
        let plaintext = response
            .open(
                &VerifyingKey::from_sec1_bytes(&B64.decode(&self.mac.mac_signing_key)?)?,
                &SecretKey::from_slice(&B64.decode(&self.mac.agreement_key)?)?,
                &self.mac.mac_id,
                &self.mac.device_id,
                &self.mac.mac_id,
                now_ms(),
            )
            .map_err(|_| key_changed(&self.mac.name))?;
        let result: Value = serde_json::from_slice(&plaintext)?;
        let status = result["status"].as_u64().filter(|s| (100..600).contains(s)).context("invalid endpoint status")?
            as u16;
        Ok((status, result.get("body").cloned().unwrap_or(result)))
    }

    /// Sends one request and returns the Mac's answer, over iroh first. If a path fails mid-way,
    /// the same sealed bytes go to the next one: the Mac runs a request ID once and replays its
    /// answer.
    pub fn request(&self, method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<(u16, Value)> {
        self.request_observed(method, path, body, timeout, None)
    }

    pub(crate) fn request_observed(&self, method: &str, path: &str, body: Option<&Value>, timeout: Duration, observer: Option<&crate::client::availability::ResponseObserver>) -> Result<(u16, Value)> {
        let (request_id, bytes) = self.seal(method, path, body, timeout)?;
        let sent = match self.over_iroh(&bytes, &request_id, timeout, observer) {
            OverIroh::Answered(answer) => return answer,
            OverIroh::NotSent => false,
            OverIroh::Lost => true,
        };
        let answer = self.over_http(&request_id, &bytes, timeout, sent, observer);
        if answer.is_ok() {
            self.learn_dial_once();
        }
        answer
    }

    fn over_http(&self, request_id: &str, bytes: &[u8], timeout: Duration, mut sent: bool, observer: Option<&crate::client::availability::ResponseObserver>) -> Result<(u16, Value)> {
        let deadline = Instant::now() + timeout;
        let name = &self.mac.name;
        for route in self.candidates() {
            let known = self.current.lock().expect("route lock poisoned").as_deref() == Some(route.as_str());
            if !known && !self.validate(&route)? {
                continue;
            }
            let wait = deadline.saturating_duration_since(Instant::now()) + Duration::from_secs(5);
            let mut request = agent(wait)
                .post(format!("http://{route}/direct/request"))
                .header("content-type", "application/json");
            if observer.is_some() { request = request.config().max_redirects(0).build(); }
            let attempt = request.send(&bytes[..]);
            let mut response = match attempt {
                Ok(response) => response,
                Err(_) => {
                    sent = true;
                    *self.current.lock().expect("route lock poisoned") = None;
                    continue;
                }
            };
            if let Some(observer) = observer { observer.received(); }
            let status = response.status().as_u16();
            let text = response.body_mut().with_config().limit(2 * 1024 * 1024).read_to_vec()?;
            if status == 200
                && let Ok(answer) = self.open(&text, request_id)
            {
                *self.current.lock().expect("route lock poisoned") = Some(route.clone());
                return Ok(answer);
            }
            sent = true;
            *self.current.lock().expect("route lock poisoned") = None;
        }
        if sent {
            return Err(anyhow::Error::new(Unanswered(trf!(
                "{name} stopped answering mid-request; check it before repeating an action",
                "{name} dejó de responder a mitad de la petición; compruébalo antes de repetir una acción"
            ))));
        }
        Err(anyhow::Error::new(Unsent(trf!("{name} is not on this network", "{name} no está en esta red"))))
    }

    /// Asks the Mac where else it can be reached, and keeps those routes, and how to dial its
    /// iroh endpoint, for next time.
    pub fn learn_routes(&self, home: &Path) -> Result<Vec<String>> {
        self.learned.store(true, Ordering::SeqCst);
        let (status, body) = self.request("GET", "/routes", None, Duration::from_secs(10))?;
        if status != 200 {
            bail!("routes unavailable (HTTP {status})");
        }
        let routes = ordered_routes(&body, &self.mac.routes);
        forget_routes_of_others(home, &self.mac.mac_id, &routes, crate::iroh_client::Dial::from_routes(&body))?;
        Ok(routes)
    }
}

/// LAN addresses first, then the tailnet address, then its name, then what was known before.
pub fn ordered_routes(body: &Value, known: &[String]) -> Vec<String> {
    let port = body["port"].as_u64().unwrap_or(DEFAULT_PORT as u64);
    let address = |ip: &str| match ip.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(_)) => format!("[{ip}]:{port}"),
        _ => format!("{ip}:{port}"),
    };
    let mut routes: Vec<String> = body["lan"].as_array().into_iter().flatten().filter_map(Value::as_str).map(address).collect();
    if let Some(ip) = body["tailscale"]["ip"].as_str() {
        routes.push(address(ip));
    }
    if let Some(name) = body["tailscale"]["name"].as_str() {
        routes.push(format!("{name}:{port}"));
    }
    for route in known {
        if !routes.contains(route) {
            routes.push(route.clone());
        }
    }
    routes
}

/// A Mac Bonjour found on this network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub route: String,
}

pub fn service_type() -> &'static str {
    if std::env::var("ALONDRA_SANDBOX").is_ok_and(|v| v == "1") { "_alondra-sandbox._tcp" } else { "_alondra._tcp" }
}

/// Lists the Macs advertising Alondra here, for a few seconds.
pub fn browse(window: Duration) -> Result<Vec<Found>> {
    let services = alondra_core::bonjour::browse(service_type(), window)
        .map_err(|error| anyhow!("Bonjour search failed: {error}"))?;
    Ok(services
        .into_iter()
        .map(|service| Found { name: service.name, route: format!("{}:{}", service.host, service.port) })
        .collect())
}

/// One pairing attempt with a Mac, from `hello` to its signed approval.
pub struct Pairing {
    pub route: String,
    pub hello: Hello,
    device_id: String,
    signing: SigningKey,
    agreement: SecretKey,
    signing_key: String,
    agreement_key: String,
    nonce: String,
    pub pairing_id: String,
    mac_nonce: String,
}

fn post_json(route: &str, path: &str, body: &Value) -> Result<(u16, Value)> {
    let mut response = agent(Duration::from_secs(10))
        .post(format!("http://{route}{path}"))
        .header("content-type", "application/json")
        .send(&serde_json::to_vec(body)?[..])?;
    let status = response.status().as_u16();
    let bytes = response.body_mut().with_config().limit(64 * 1024).read_to_vec()?;
    Ok((status, serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
}

fn refusal(status: u16, body: &Value) -> anyhow::Error {
    match body["error"].as_str() {
        Some("pairing_denied") => anyhow!(tr!("That Mac denied this terminal earlier.", "Ese Mac ya rechazó este terminal.")),
        Some("pairing_busy") => anyhow!(tr!("That Mac has too many pairing requests right now; try again in a few minutes.", "Ese Mac tiene demasiadas peticiones de emparejamiento ahora; vuelve a intentarlo en unos minutos.")),
        Some("pairing_expired") => anyhow!(tr!("The pairing request expired; start again.", "La petición de emparejamiento caducó; empieza de nuevo.")),
        _ => anyhow!("pairing refused (HTTP {status}): {body}"),
    }
}

impl Pairing {
    /// Sends this terminal's fresh keys and commitment, then reveals its nonce.
    pub fn start(route: &str, hello: Hello, terminal_name: &str) -> Result<Self> {
        let signing = SigningKey::from_slice(&random::<32>()?)?;
        let agreement = SecretKey::from_slice(&random::<32>()?)?;
        let signing_key = B64.encode(signing.verifying_key().to_sec1_point(false).as_bytes());
        let agreement_key = B64.encode(agreement.public_key().to_sec1_point(false).as_bytes());
        let device_id = format!("client-{}", B64.encode(random::<16>()?));
        let nonce = protocol::random_nonce()?;
        let commitment = protocol::commitment(&nonce, &device_id, &signing_key, &agreement_key)?;
        let mac_agreement = PublicKey::from_sec1_bytes(&B64.decode(&hello.agreement_key)?)?;
        let seal = |request_id: &str, plain: Value| -> Result<Envelope> {
            Ok(Envelope::seal_random(
                EnvelopeContext {
                    route: &hello.mac_id,
                    sender: &device_id,
                    recipient: &hello.mac_id,
                    request_id,
                    kind: "pairing",
                    expires_at_ms: now_ms() + 120_000,
                },
                &serde_json::to_vec(&plain)?,
                &signing,
                &mac_agreement,
            )?)
        };
        let start = seal(
            &format!("pair-{}", B64.encode(random::<12>()?)),
            json!({"device_id": device_id, "name": terminal_name, "signing_key": signing_key,
                "agreement_key": agreement_key, "commitment": commitment}),
        )?;
        let (status, started) = post_json(route, "/direct/pairing", &json!({"signing_key": signing_key, "envelope": start}))?;
        if status != 200 {
            return Err(refusal(status, &started));
        }
        let pairing_id = started["pairing_id"].as_str().context("pairing id missing")?.to_owned();
        let mac_nonce = started["mac_nonce"].as_str().context("Mac nonce missing")?.to_owned();
        let reveal = seal(
            &format!("pair-{}", B64.encode(random::<12>()?)),
            json!({"pairing_id": pairing_id, "client_nonce": nonce}),
        )?;
        let (status, body) = post_json(route, &format!("/direct/pairing/{pairing_id}/reveal"), &json!({"envelope": reveal}))?;
        if status != 200 {
            return Err(refusal(status, &body));
        }
        Ok(Self {
            route: route.into(),
            hello,
            device_id,
            signing,
            agreement,
            signing_key,
            agreement_key,
            nonce,
            pairing_id,
            mac_nonce,
        })
    }

    fn transcript(&self) -> Transcript<'_> {
        Transcript {
            pairing_id: &self.pairing_id,
            mac_id: &self.hello.mac_id,
            mac_signing_key: &self.hello.signing_key,
            mac_agreement_key: &self.hello.agreement_key,
            device_id: &self.device_id,
            client_signing_key: &self.signing_key,
            client_agreement_key: &self.agreement_key,
            client_nonce: &self.nonce,
            mac_nonce: &self.mac_nonce,
        }
    }

    /// The code this terminal shows; the Mac shows the same one if nobody is in between.
    pub fn code(&self) -> Result<String> {
        Ok(self.transcript().code()?)
    }

    /// Waits for the decision on the Mac and verifies its signed approval against `hello`.
    pub fn wait(self) -> Result<DirectMac> {
        let until = Instant::now() + PAIRING_WAIT;
        let mac_key = VerifyingKey::from_sec1_bytes(&B64.decode(&self.hello.signing_key)?)?;
        loop {
            let mut response = agent(Duration::from_secs(10))
                .get(format!("http://{}/direct/pairing/{}", self.route, self.pairing_id))
                .call()?;
            let status: Value = serde_json::from_slice(&response.body_mut().with_config().limit(64 * 1024).read_to_vec()?)
                .unwrap_or(Value::Null);
            match status["state"].as_str() {
                Some("approved") => {
                    let approval: Approval = serde_json::from_value(status["approval"].clone())?;
                    let signature = status["signature"].as_str().context("approval signature missing")?;
                    approval
                        .verify(signature, &mac_key, &self.transcript(), now_ms())
                        .map_err(|error| anyhow!("the Mac's approval does not verify ({error}); nothing was saved"))?;
                    return Ok(DirectMac {
                        mac_id: self.hello.mac_id.clone(),
                        name: self.hello.name.clone(),
                        mac_signing_key: self.hello.signing_key.clone(),
                        mac_agreement_key: self.hello.agreement_key.clone(),
                        device_id: self.device_id.clone(),
                        signing_key: B64.encode(self.signing.to_bytes()),
                        agreement_key: B64.encode(self.agreement.to_bytes()),
                        routes: vec![self.route.clone()],
                    });
                }
                Some("denied") => bail!(tr!("The Mac denied this terminal.", "El Mac ha rechazado este terminal.")),
                Some("expired") => bail!(tr!("Nobody approved it on the Mac in time; start again.", "Nadie lo aprobó en el Mac a tiempo; empieza de nuevo.")),
                _ if Instant::now() >= until => bail!(tr!("Nobody approved it on the Mac in time; start again.", "Nadie lo aprobó en el Mac a tiempo; empieza de nuevo.")),
                _ => std::thread::sleep(Duration::from_secs(1)),
            }
        }
    }
}

fn with_port(host: &str) -> String {
    let bracketed_v6 = host.starts_with('[') && host.contains("]:");
    let plain_v6 = host.parse::<std::net::Ipv6Addr>().is_ok();
    if plain_v6 {
        format!("[{host}]:{DEFAULT_PORT}")
    } else if bracketed_v6 || (host.contains(':') && !host.starts_with('[')) {
        host.to_owned()
    } else {
        format!("{host}:{DEFAULT_PORT}")
    }
}

fn choose(found: &[Found]) -> Result<Found> {
    if found.is_empty() {
        bail!(tr!(
            "No Mac with Alondra found on this network. Is it on the same Wi-Fi? You can also use: alondra mac connect --host <address>",
            "No he encontrado ningún Mac con Alondra en esta red. ¿Está en la misma Wi-Fi? También puedes usar: alondra mac connect --host <dirección>"
        ));
    }
    for (index, mac) in found.iter().enumerate() {
        println!("  {}. {}  ({})", index + 1, mac.name, mac.route);
    }
    print!("{}", tr!("Which Mac? [1] ", "¿Qué Mac? [1] "));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let index = line.trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
    found.get(index).cloned().context("no such Mac")
}

/// `alondra mac connect` without a Mac ID: find a Mac on this network (or use `--host`), compare the
/// code on both screens, and keep the pairing once the Mac approves it.
pub fn connect(host: Option<&str>, repair: bool) -> Result<()> {
    let home = crate::client::config().home.clone();
    let route = match host {
        Some(host) => with_port(host),
        None => {
            println!("{}", tr!("Looking for Macs on this network…", "Buscando Macs en esta red…"));
            choose(&browse(Duration::from_secs(3))?)?.route
        }
    };
    let hello = hello(&route).with_context(|| trf!("{route} does not answer as an Alondra Mac", "{route} no responde como un Mac con Alondra"))?;
    if let Some(known) = load_from(&home)?.into_iter().find(|mac| mac.mac_id == hello.mac_id) {
        match check(&known, &hello) {
            Check::Same if !repair => {
                let name = &known.name;
                println!("{}", trf!("Already paired with {name}.", "Ya está emparejado con {name}."));
                return Ok(());
            }
            Check::KeyChanged if !repair => return Err(key_changed(&known.name)),
            _ => {}
        }
    }
    let terminal = format!("Terminal ({})", crate::client::computer_name());
    let pairing = Pairing::start(&route, hello, &terminal)?;
    let (code, name) = (pairing.code()?, pairing.hello.name.clone());
    println!(
        "{}",
        trf!(
            "Code: {code}. Check that {name} shows the same code and approve it there.",
            "Código: {code}. Comprueba que {name} muestra el mismo código y apruébalo allí."
        )
    );
    let mac = pairing.wait()?;
    let name = mac.name.clone();
    remember(&home, mac.clone())?;
    let connection = Connection::new(mac);
    if let Err(error) = connection.learn_routes(&home) {
        eprintln!("{}", trf!("Paired, but could not learn other routes yet: {error}", "Emparejado, pero aún no he podido aprender otras rutas: {error}"));
    }
    connection.greet_over_iroh();
    println!("{}", trf!("Paired with {name}.", "Emparejado con {name}."));
    Ok(())
}

fn route_name(kind: Option<RouteKind>) -> &'static str {
    match kind {
        Some(RouteKind::Lan) => tr!("the local network", "la red local"),
        Some(RouteKind::Tailscale) => "Tailscale",
        Some(RouteKind::Direct) => tr!("a direct connection", "conexión directa"),
        Some(RouteKind::Relay) => tr!("the relay", "el relay"),
        None => tr!("no route", "ninguna ruta"),
    }
}

/// `alondra mac ping`: how long opening the connection and each read of a paired Mac take, and
/// which way they go.
pub fn ping(name: Option<&str>, count: u32, json_output: bool) -> Result<()> {
    let home = crate::client::config().home.clone();
    let macs = load_from(&home)?;
    let mac = match name {
        Some(name) => macs.into_iter().find(|mac| mac.mac_id == name || mac.name.eq_ignore_ascii_case(name)),
        None if macs.len() == 1 => macs.into_iter().next(),
        None => bail!(tr!(
            "Say which Mac: alondra mac ping <name>; see them with alondra mac list",
            "Di qué Mac: alondra mac ping <nombre>; míralos con alondra mac list"
        )),
    }
    .with_context(|| tr!("This terminal is not connected to that Mac; see them with alondra mac list", "Este terminal no está conectado a ese Mac; míralos con alondra mac list"))?;
    let connection = Connection::at(&home, mac);
    let started = Instant::now();
    let over_iroh = connection.greet_over_iroh();
    let connect_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut reads = Vec::new();
    for _ in 0..count {
        let started = Instant::now();
        let (status, _) = connection.request("GET", "/capabilities", None, Duration::from_secs(15))?;
        if status != 200 {
            bail!("{} answered {status}", connection.mac.name);
        }
        reads.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    reads.sort_by(f64::total_cmp);
    let median = reads[reads.len() / 2];
    let p95 = reads[(reads.len() * 95).div_ceil(100) - 1];
    let route = connection.route();
    let kind = route.as_ref().map(|(_, kind)| *kind);
    if json_output {
        let kind = kind.map(|kind| format!("{kind:?}").to_lowercase());
        println!("{}", json!({"mac": connection.mac.name, "iroh": over_iroh, "connect_ms": connect_ms, "route": kind,
            "via": route.map(|(via, _)| via), "reads": count, "median_ms": median, "p95_ms": p95}));
        return Ok(());
    }
    let (name, way) = (&connection.mac.name, route_name(kind));
    let (median, p95) = (format!("{median:.1}"), format!("{p95:.1}"));
    println!("{}", trf!(
        "{count} reads of {name} through {way}: median {median} ms, p95 {p95} ms",
        "{count} lecturas de {name} por {way}: mediana {median} ms, p95 {p95} ms"
    ));
    if over_iroh {
        let connect = format!("{connect_ms:.0}");
        println!("{}", trf!("Opening the iroh connection took {connect} ms", "Abrir la conexión iroh tardó {connect} ms"));
    } else {
        println!("{}", tr!("No iroh connection: the reads went over the HTTP route", "Sin conexión iroh: las lecturas fueron por la ruta HTTP"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "direct_client_iroh_tests.rs"]
pub(crate) mod iroh_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(id: &str, signing: &str, agreement: &str) -> DirectMac {
        DirectMac {
            mac_id: id.into(),
            name: "Mac".into(),
            mac_signing_key: signing.into(),
            mac_agreement_key: agreement.into(),
            device_id: "client-1".into(),
            signing_key: "s".into(),
            agreement_key: "a".into(),
            routes: vec!["192.168.1.2:47810".into()],
        }
    }

    fn hello_of(id: &str, signing: &str, agreement: &str) -> Hello {
        Hello { mac_id: id.into(), name: "Mac".into(), signing_key: signing.into(), agreement_key: agreement.into() }
    }

    #[test]
    fn a_route_is_used_only_for_the_pinned_mac_and_a_changed_key_is_never_accepted() {
        let pinned = mac("mac-a", "sk", "ak");
        assert_eq!(check(&pinned, &hello_of("mac-a", "sk", "ak")), Check::Same);
        assert_eq!(check(&pinned, &hello_of("mac-b", "sk2", "ak2")), Check::OtherMac);
        assert_eq!(check(&pinned, &hello_of("mac-a", "other", "ak")), Check::KeyChanged);
        assert_eq!(check(&pinned, &hello_of("mac-a", "sk", "other")), Check::KeyChanged);
    }

    #[test]
    fn learned_routes_put_lan_first_then_tailnet_then_what_was_known() {
        let body = json!({"port": 47810, "lan": ["192.168.1.20", "fd12::1"], "tailscale": {"ip": "100.101.1.2", "name": "mac.tail.ts.net"}});
        let routes = ordered_routes(&body, &["mac.local:47810".into(), "192.168.1.20:47810".into()]);
        assert_eq!(
            routes,
            ["192.168.1.20:47810", "[fd12::1]:47810", "100.101.1.2:47810", "mac.tail.ts.net:47810", "mac.local:47810"]
        );
        assert_eq!(route_kind("100.101.1.2:47810"), RouteKind::Tailscale);
        assert_eq!(route_kind("mac.tail.ts.net:47810"), RouteKind::Tailscale);
        assert_eq!(route_kind("192.168.1.20:47810"), RouteKind::Lan);
        assert_eq!(route_kind("[fd12::1]:47810"), RouteKind::Lan);
    }

    #[test]
    fn a_host_without_port_gets_the_direct_port() {
        assert_eq!(with_port("192.168.1.2"), "192.168.1.2:47810");
        assert_eq!(with_port("mac.local:47999"), "mac.local:47999");
        assert_eq!(with_port("fd12::1"), "[fd12::1]:47810");
        assert_eq!(with_port("[fd12::1]:47999"), "[fd12::1]:47999");
    }

    #[test]
    fn the_store_is_owner_only_and_keeps_one_pairing_per_mac() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-direct-store-{}", B64.encode(random::<6>().unwrap())));
        fs::create_dir_all(&home).unwrap();
        remember(&home, mac("mac-a", "sk", "ak")).unwrap();
        remember(&home, mac("mac-b", "sk2", "ak2")).unwrap();
        remember(&home, mac("mac-a", "sk3", "ak3")).unwrap();
        let macs = load_from(&home).unwrap();
        assert_eq!(macs.len(), 2);
        assert_eq!(macs.iter().find(|m| m.mac_id == "mac-a").unwrap().mac_signing_key, "sk3");
        assert_eq!(fs::metadata(store_path(&home)).unwrap().permissions().mode() & 0o777, 0o600);
        fs::set_permissions(store_path(&home), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_from(&home).is_err(), "a readable store is refused");
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn forget_takes_a_mac_off_by_id_or_by_name_and_keeps_the_rest() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&*home).unwrap();
        let named = |id: &str, name: &str| DirectMac { name: name.into(), ..mac(id, "sk", "ak") };
        remember(&home, named("mac-a", "Mac mini")).unwrap();
        remember(&home, named("mac-b", "Studio")).unwrap();
        remember(&home, named("mac-c", "Work")).unwrap();
        assert_eq!(forget(&home, "mac mini").unwrap(), ["mac-a"]);
        assert_eq!(forget(&home, "mac-b").unwrap(), ["mac-b"]);
        assert!(forget(&home, "Nowhere").unwrap().is_empty());
        let left: Vec<String> = load_from(&home).unwrap().into_iter().map(|m| m.mac_id).collect();
        assert_eq!(left, ["mac-c"]);
    }
}
