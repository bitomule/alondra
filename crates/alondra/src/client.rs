use std::sync::OnceLock;
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::domain::{AttachmentRef, Command, CommandEnvelope, CommandOutcome, DataEpoch, DomainError, PROTOCOL_VERSION, Revision};
use alondra_core::views::{View as ViewDocument, VIEWS_PROTOCOL};
use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::Value;

static CONFIG: OnceLock<Config> = OnceLock::new();

pub(crate) mod availability;

/// The name the user gave this computer, as pairing requests show it on the Mac.
pub fn computer_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

/// The effective configuration; a broken `config.toml` falls back to defaults here and is
/// reported by `alondra doctor`.
pub fn config() -> &'static Config {
    CONFIG.get_or_init(|| Config::load().unwrap_or_else(|_| Config::defaults()))
}

#[derive(Clone)]
pub struct Client {
    base: String,
    agent: ureq::Agent,
    pairing: Option<std::sync::Arc<Pairing>>,
    service: Option<std::sync::Arc<crate::service_client::Profile>>,
    direct: Option<std::sync::Arc<crate::direct_client::Connection>>,
    timeout: Duration,
    protocol: u32,
    mcp_operation: Option<String>,
    capabilities: std::sync::Arc<std::sync::Mutex<alondra_core::views::ClientCapabilities>>,
    response_observer: Option<availability::ResponseObserver>,
}

/// The version the local daemon stamped on the first answer this process got from it.
static SERVICE_VERSION: OnceLock<String> = OnceLock::new();

pub fn service_version() -> Option<&'static str> {
    SERVICE_VERSION.get().map(String::as_str)
}

/// Which Mac a TUI instance talks to: this one over loopback, or one paired directly.
#[derive(Clone)]
pub enum Target {
    Local,
    Direct(std::sync::Arc<crate::direct_client::Connection>),
}

impl Target {
    pub fn is_local(&self) -> bool {
        matches!(self, Target::Local)
    }
}

/// Long-poll reply for the views feed. `data_epoch` is carried explicitly so actions
/// can be bound to the exact domain generation that produced a view.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ViewsFeed {
    pub protocol: u32,
    pub cursor: String,
    pub data_epoch: String,
    pub status: alondra_core::views::Status,
    #[serde(default)]
    pub stale: Vec<String>,
    #[serde(default)]
    pub reset: bool,
    #[serde(default)]
    pub superseded: bool,
    #[serde(default)]
    pub service: Option<alondra_core::views::Service>,
}

pub struct PullRequestDiff {
    pub text: String,
    pub truncated: bool,
    /// The viewed mark GitHub keeps for each file; none when the daemon or GitHub gave none.
    pub files: Option<Vec<FileMark>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileViewed {
    Viewed,
    Unviewed,
    /// Viewed once, then a push changed the file.
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct FileMark {
    pub path: String,
    pub viewed: FileViewed,
}

#[derive(Clone)]
struct Signer {
    device: String,
    key: p256::ecdsa::SigningKey,
}

static LAST_TIMESTAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Keep timestamps ordered within this process; signing entropy distinguishes separate processes.
fn unique_timestamp() -> u64 {
    use std::sync::atomic::Ordering::SeqCst;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let mut last = LAST_TIMESTAMP.load(SeqCst);
    loop {
        let next = now.max(last + 1);
        match LAST_TIMESTAMP.compare_exchange(last, next, SeqCst, SeqCst) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

const KEY_FILE: &str = "tui-key.pem";
const DEVICE_FILE: &str = "tui-device";

struct SigningEntropy(ring::rand::SystemRandom);
impl p256::elliptic_curve::rand_core::TryRng for SigningEntropy {
    type Error = std::io::Error;
    fn try_next_u32(&mut self) -> std::io::Result<u32> {
        let mut bytes = [0; 4]; self.try_fill_bytes(&mut bytes)?; Ok(u32::from_le_bytes(bytes))
    }
    fn try_next_u64(&mut self) -> std::io::Result<u64> {
        let mut bytes = [0; 8]; self.try_fill_bytes(&mut bytes)?; Ok(u64::from_le_bytes(bytes))
    }
    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> std::io::Result<()> {
        use ring::rand::SecureRandom as _;
        self.0.fill(bytes).map_err(|_| std::io::Error::other("system entropy unavailable"))
    }
}
impl p256::elliptic_curve::rand_core::TryCryptoRng for SigningEntropy {}

impl Signer {
    fn sign(&self, method: &str, path: &str, body: &[u8]) -> Result<[(&'static str, String); 3]> {
        self.sign_at(method, path, body, unique_timestamp())
    }

    fn sign_at(&self, method: &str, path: &str, body: &[u8], timestamp: u64) -> Result<[(&'static str, String); 3]> {
        self.sign_with_entropy(method, path, body, timestamp, &mut SigningEntropy(ring::rand::SystemRandom::new()))
    }

    fn sign_with_entropy<R: p256::elliptic_curve::rand_core::TryCryptoRng + ?Sized>(
        &self, method: &str, path: &str, body: &[u8], timestamp: u64, entropy: &mut R,
    ) -> Result<[(&'static str, String); 3]> {
        use base64::Engine as _;
        use p256::ecdsa::signature::RandomizedSigner as _;
        use sha2::Digest as _;
        let timestamp = timestamp.to_string();
        let digest: String = sha2::Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
        let message = format!("{method}\n{path}\n{timestamp}\n{digest}");
        let signature: p256::ecdsa::Signature = self.key.try_sign_with_rng(entropy, message.as_bytes())
            .map_err(|_| anyhow!(tr!("Could not sign the local request", "No se pudo firmar la solicitud local")))?;
        Ok([
            ("x-alondra-device", self.device.clone()),
            ("x-alondra-timestamp", timestamp),
            ("x-alondra-signature", base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes())),
        ])
    }

    fn same_pair(&self, other: &Signer) -> bool {
        self.device == other.device && self.key.verifying_key() == other.key.verifying_key()
    }
}

const PAIRING_LOCK_FILE: &str = "tui-pairing.lock";

static PAIRING: std::sync::LazyLock<std::sync::Arc<Pairing>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(Pairing::at(config().home.clone())));

/// The key this terminal signs the user's answers with, paired with alondrad on first use. Every
/// client of the process shares it, and `tui-pairing.lock` keeps other processes from pairing at
/// the same time, so the home never holds the key of one pairing with the device of another.
struct Pairing {
    home: std::path::PathBuf,
    signer: std::sync::Mutex<Option<Signer>>,
    replaced: std::sync::atomic::AtomicBool,
}

impl Pairing {
    fn at(home: std::path::PathBuf) -> Self {
        Pairing { home, signer: std::sync::Mutex::new(None), replaced: std::sync::atomic::AtomicBool::new(false) }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Option<Signer>> {
        self.signer.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn current(&self) -> Option<Signer> {
        self.cache().clone()
    }

    fn signer(&self, unsigned: &Client) -> Result<Signer> {
        let mut cache = self.cache();
        if let Some(signer) = cache.as_ref() {
            return Ok(signer.clone());
        }
        let _lock = self.lock()?;
        let signer = match self.stored() {
            Some(stored) => stored,
            None => self.pair(unsigned)?,
        };
        *cache = Some(signer.clone());
        Ok(signer)
    }

    /// alondrad knows the stored device but not with the stored key, as a home paired twice at
    /// once was left. Pairs again, once per process, unless another process already did.
    fn replace_rejected(&self, rejected: &Signer, unsigned: &Client) -> Result<Option<Signer>> {
        let mut cache = self.cache();
        let _lock = self.lock()?;
        let signer = match self.stored() {
            Some(stored) if !stored.same_pair(rejected) => stored,
            _ if self.replaced.swap(true, std::sync::atomic::Ordering::SeqCst) => return Ok(None),
            _ => self.pair(unsigned)?,
        };
        *cache = Some(signer.clone());
        Ok(Some(signer))
    }

    fn lock(&self) -> Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(&self.home)?;
        let file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600)
            .open(self.home.join(PAIRING_LOCK_FILE))?;
        file.lock()?;
        Ok(file)
    }

    fn stored(&self) -> Option<Signer> {
        use p256::pkcs8::DecodePrivateKey;
        let device = std::fs::read_to_string(self.home.join(DEVICE_FILE)).ok()?.trim().to_string();
        let key = p256::ecdsa::SigningKey::from_pkcs8_pem(&std::fs::read_to_string(self.home.join(KEY_FILE)).ok()?).ok()?;
        (!device.is_empty()).then_some(Signer { device, key })
    }

    /// The device file goes first and comes back last: a cut in between leaves no device, and
    /// the next start pairs again instead of signing with a key alondrad does not have.
    fn pair(&self, unsigned: &Client) -> Result<Signer> {
        use base64::Engine as _;
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        let mut seed = [0u8; 32];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut seed)?;
        let key = p256::ecdsa::SigningKey::from_slice(&seed).map_err(|e| anyhow!("key: {e}"))?;
        let code = unsigned.post("/pairing", &serde_json::json!({}))?.get("code").and_then(Value::as_str).map(str::to_string).context("no pairing code")?;
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes());
        let host = computer_name();
        let paired = unsigned.post("/pair", &serde_json::json!({"code": code, "device_name": format!("Terminal ({host})"), "public_key": public}))?;
        let device = paired.get("device_id").and_then(Value::as_str).context("no device id")?.to_string();
        let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(|e| anyhow!("key: {e}"))?;
        let device_path = self.home.join(DEVICE_FILE);
        match std::fs::remove_file(&device_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        replace_private(&self.home.join(KEY_FILE), pem.as_bytes())?;
        replace_private(&device_path, device.as_bytes())?;
        Ok(Signer { device, key })
    }
}

fn replace_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

impl Client {
    pub fn new(timeout: Duration) -> Self {
        let c = config();
        let host = if c.listen.is_unspecified() || c.listen.is_loopback() { "127.0.0.1".to_string() } else { c.listen.to_string() };
        let port = c.port;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        Client { base: format!("http://{host}:{port}"), agent, pairing: None, service: None, direct: None, timeout, protocol: VIEWS_PROTOCOL, mcp_operation: None, capabilities: Default::default(), response_observer: None }
    }

    #[cfg(test)]
    pub(crate) fn at(base: String) -> Self {
        let mut client = Self::new(Duration::from_secs(5));
        client.base = base;
        client
    }

    pub(crate) fn bounded(&self, timeout: Duration) -> Self {
        let mut client = Self::new(timeout);
        client.base = self.base.clone();
        client.pairing = self.pairing.clone();
        client.service = self.service.clone();
        client.direct = self.direct.clone();
        client.protocol = self.protocol;
        client.capabilities = self.capabilities.clone();
        client
    }

    /// A client whose requests are signed as the user: for the TUI and the commands the user
    /// types. Falls back to unsigned (and says so on stderr) when pairing is impossible.
    pub fn for_user(timeout: Duration) -> Self {
        let mut client = Self::new(timeout);
        match PAIRING.signer(&client) {
            Ok(_) => client.pairing = Some(PAIRING.clone()),
            Err(e) => eprintln!("{}", trf!("alondra: answers will go unsigned ({e})", "alondra: las respuestas irán sin firmar ({e})")),
        }
        client
    }

    /// Only interactive TUI requests consult its explicitly selected remote destination.
    pub fn for_tui(timeout: Duration) -> Self {
        if let Some(profile) = crate::service_client::active() {
            let mut client = Self::new(timeout.max(Duration::from_secs(15)));
            client.service = Some(profile);
            client
        } else { Self::for_user(timeout) }
    }

    /// A client for one TUI instance's Mac. A direct Mac gets the longer wait its network needs.
    pub fn for_target(target: &Target, timeout: Duration) -> Self {
        match target {
            Target::Local => Self::for_tui(timeout),
            Target::Direct(connection) => {
                let mut client = Self::new(timeout.max(Duration::from_secs(15)));
                client.direct = Some(connection.clone());
                client
            }
        }
    }

    /// The views protocol this client asks for, once the daemon's range is known.
    pub fn with_protocol(mut self, protocol: u32) -> Self {
        self.protocol = protocol;
        self
    }

    pub fn with_mcp_operation(&self, identity: &str) -> Self {
        assert!(!identity.is_empty() && identity.len() <= 1024 && identity.bytes().all(|byte| byte.is_ascii_hexdigit()), "invalid MCP operation identity");
        Self { base: self.base.clone(), agent: self.agent.clone(), pairing: self.pairing.clone(), service: self.service.clone(), direct: self.direct.clone(), timeout: self.timeout,
            protocol: self.protocol, mcp_operation: Some(identity.into()), capabilities: self.capabilities.clone(), response_observer: None }
    }

    pub fn operation_id(&self) -> Option<&str> { self.mcp_operation.as_deref() }

    fn headers<B>(&self, mut req: ureq::RequestBuilder<B>, method: &str, path: &str, body: &[u8], signer: Option<&Signer>) -> Result<ureq::RequestBuilder<B>> {
        if self.response_observer.is_some() { req = req.config().max_redirects(0).build(); }
        req = req.header("x-alondra-client-version", env!("CARGO_PKG_VERSION"));
        if let Some(operation) = self.operation_id() { req = req.header("x-alondra-mcp-operation", operation); }
        if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") { req = req.header("x-alondra-orchestrator-execution", run); }
        if let Ok(sid) = std::env::var("ALONDRA_SESSION_ID").or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID")) { req = req.header("x-alondra-session", sid); }
        match signer {
            Some(s) => Ok(s.sign(method, path, body)?.into_iter().fold(req, |r, (k, v)| r.header(k, v))),
            None => Ok(req),
        }
    }

    /// The raw answer, a refusal included: domain commands read their typed error from it.
    pub(crate) fn exchange(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        if let Some(observer) = &self.response_observer { observer.begin_request(); }
        if let Some(service) = &self.service {
            return service.request_observed(method, path, body, self.timeout, self.response_observer.as_ref());
        }
        if let Some(direct) = &self.direct {
            return direct.request_observed(method, path, body, self.timeout, self.response_observer.as_ref());
        }
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let (status, answer) = self.local(method, path, "application/json", &bytes)?;
        Ok((status, serde_json::from_slice(&answer).unwrap_or(Value::Null)))
    }

    /// A request to this Mac's alondrad, sent once more after pairing again when alondrad
    /// rejects the stored key: a 401 is refused before anything runs.
    fn local(&self, method: &str, path: &str, content_type: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
        let signer = self.pairing.as_ref().and_then(|pairing| pairing.current());
        let (status, answer) = self.send_local(method, path, content_type, body, signer.as_ref())?;
        if let (Some(pairing), Some(rejected)) = (&self.pairing, &signer)
            && status == 401
            && serde_json::from_slice::<Value>(&answer).is_ok_and(|value| value["error"] == "wrong signature")
            && let Ok(Some(fresh)) = pairing.replace_rejected(rejected, &self.unsigned())
        {
            return self.send_local(method, path, content_type, body, Some(&fresh));
        }
        Ok((status, answer))
    }

    fn send_local(&self, method: &str, path: &str, content_type: &str, body: &[u8], signer: Option<&Signer>) -> Result<(u16, Vec<u8>)> {
        let url = format!("{}{path}", self.base);
        let not_responding = tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)");
        let mut resp = match method {
            "POST" => self.headers(self.agent.post(url).header("content-type", content_type), method, path, body, signer)?.send(body),
            "PUT" => self.headers(self.agent.put(url).header("content-type", content_type), method, path, body, signer)?.send(body),
            "DELETE" => self.headers(self.agent.delete(url), method, path, b"", signer)?.call(),
            _ => self.headers(self.agent.get(url), method, path, b"", signer)?.call(),
        }
        .context(not_responding)?;
        if let Some(observer) = &self.response_observer { observer.received(); }
        if let Some(version) = resp.headers().get("x-alondra-version").and_then(|v| v.to_str().ok()) {
            let _ = SERVICE_VERSION.set(version.to_owned());
        }
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().with_config().limit(MAX_ATTACHMENT_BYTES + 1024 * 1024).read_to_vec()?))
    }

    fn unsigned(&self) -> Client {
        Client { base: self.base.clone(), agent: self.agent.clone(), pairing: None, service: None, direct: None, timeout: self.timeout, protocol: self.protocol, mcp_operation: None, capabilities: Default::default(), response_observer: None }
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let (status, value) = self.exchange(method, path, body)?;
        if status >= 400 {
            return Err(failure(status, &value));
        }
        Ok(value)
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(serde_json::from_value(self.call("GET", path, None)?)?)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("POST", path, Some(body))
    }

    pub fn put(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("PUT", path, Some(body))
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        self.call("DELETE", path, None).map(|_| ())
    }

    /// Fetch one server-rendered TUI view. The key's path is one encoded segment, because card
    /// identifiers may contain reserved characters; its options (`section`, `after`, `project`…)
    /// travel in the query next to the protocol's own parameters.
    pub fn views(&self, key: &str, known: Option<&str>) -> Result<ViewDocument> {
        let view = self.view_with_capabilities(key, known)?;
        if self.observe_capabilities(view.service.as_ref()) { return self.view_with_capabilities(key, None); }
        Ok(view)
    }

    fn view_with_capabilities(&self, key: &str, known: Option<&str>) -> Result<ViewDocument> {
        let (base, options) = key.split_once('?').unwrap_or((key, ""));
        let mut query = vec![("protocol".to_owned(), self.protocol.to_string()), ("client".to_owned(), "tui".to_owned()), ("lang".to_owned(), lang_name(crate::i18n::lang()).to_owned())];
        if let Some(version) = known { query.push(("known".to_owned(), version.to_owned())); }
        let capabilities = self.capabilities.lock().unwrap_or_else(|poison| poison.into_inner()).query();
        if !capabilities.is_empty() { query.push(("capabilities".into(), capabilities)); }
        query.extend(url::form_urlencoded::parse(options.as_bytes()).into_owned());
        let query = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(query).finish();
        let path = format!("/views/{}?{query}", crate::mcp::urlencode(base));
        self.get(&path)
    }

    /// Poll changes for the open TUI views. Starting a newer request for the same device
    /// supersedes this one at the daemon.
    pub fn feed(&self, cursor: Option<&str>, views: &[(&str, &str)], wait: u64) -> Result<ViewsFeed> {
        let reply = self.feed_with_capabilities(cursor, views, wait)?;
        if self.observe_capabilities(reply.service.as_ref()) {
            let mut fresh = self.feed_with_capabilities(None, &[], 0)?;
            fresh.reset = true;
            return Ok(fresh);
        }
        Ok(reply)
    }

    fn observe_capabilities(&self, service: Option<&alondra_core::views::Service>) -> bool {
        let supported = service.map(|service| service.capabilities.iter().filter(|cap| matches!(cap.as_str(), "provider_accounts" | "model_selection" | "account_presentation")).cloned().collect::<Vec<_>>().join(",")).unwrap_or_default();
        let next = alondra_core::views::ClientCapabilities::parse(&supported).unwrap_or_default();
        let mut capabilities = self.capabilities.lock().unwrap_or_else(|poison| poison.into_inner());
        if *capabilities == next { return false; }
        *capabilities = next;
        true
    }

    fn feed_with_capabilities(&self, cursor: Option<&str>, views: &[(&str, &str)], wait: u64) -> Result<ViewsFeed> {
        self.get(&self.feed_path(cursor, views, wait))
    }

    fn feed_path(&self, cursor: Option<&str>, views: &[(&str, &str)], wait: u64) -> String {
        let mut query = vec![
            ("protocol", self.protocol.to_string()),
            ("client", "tui".to_owned()),
            ("lang", lang_name(crate::i18n::lang()).to_owned()),
            ("wait", wait.min(50).to_string()),
        ];
        let capabilities = self.capabilities.lock().unwrap_or_else(|poison| poison.into_inner()).query();
        if !capabilities.is_empty() { query.push(("capabilities", capabilities)); }
        if let Some(cursor) = cursor { query.push(("cursor", cursor.to_owned())); }
        if !views.is_empty() {
            let open = views.iter().map(|(key, version)| format!("{key}@{version}")).collect::<Vec<_>>().join(",");
            query.push(("views", open));
        }
        let query = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(query).finish();
        format!("/changes?{query}")
    }

    /// Submit a renderer-provided action, keeping action identifiers opaque to the TUI.
    pub fn action(&self, data_epoch: &str, action_id: &str, option_id: Option<&str>, input: Value) -> Result<Value> {
        self.post("/actions", &Self::action_body(data_epoch, action_id, option_id, input))
    }

    pub fn action_body(data_epoch: &str, action_id: &str, option_id: Option<&str>, input: Value) -> Value {
        let mut body = serde_json::json!({
            "protocol": VIEWS_PROTOCOL,
            "command_id": new_views_command_id(),
            "data_epoch": data_epoch,
            "client": "tui",
            "lang": lang_name(crate::i18n::lang()),
            "action_id": action_id,
            "input": input,
        });
        if let Some(option_id) = option_id { body["option_id"] = Value::String(option_id.to_owned()); }
        body
    }

    /// Producers only ever dial the daemon on this Mac; tests dial their fake one.
    fn domain_url(&self, path: &str) -> String {
        if cfg!(test) {
            return format!("{}{path}", self.base);
        }
        format!("http://127.0.0.1:{}{path}", config().port)
    }

    /// Domain producers use their daemon-issued bearer, never a locally paired User key.
    pub fn domain_get(&self, path: &str, bearer: &str) -> Result<Value> {
        let url = self.domain_url(path);
        let resp = self.agent.get(url).header("authorization", format!("Bearer {bearer}")).call()?;
        answer(resp)
    }

    pub fn domain_post(&self, path: &str, bearer: &str, body: &Value) -> Result<Value> {
        self.domain_call("POST", path, bearer, Some(body))
    }

    pub fn domain_call(&self, method: &str, path: &str, bearer: &str, body: Option<&Value>) -> Result<Value> {
        let url = self.domain_url(path);
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let mut request = ureq::http::Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .header("x-alondra-client-version", env!("CARGO_PKG_VERSION"));
        if let Some(operation) = self.operation_id() {
            request = request.header("x-alondra-mcp-operation", operation);
        }
        let response = self.agent.run(request.body(bytes)?)?;
        let status = response.status().as_u16();
        let daemon = response.headers().get("x-alondra-version").and_then(|v| v.to_str().ok()).map(str::to_owned);
        answer(response).map_err(|refusal| match daemon.filter(|daemon| refused_for_being_behind(status, daemon)) {
            Some(daemon) => anyhow::Error::new(DaemonBehind { daemon, refusal }),
            None => refusal,
        })
    }

    /// A refusal is the daemon's typed answer; anything else leaves the outcome unknown.
    pub fn command(&self, envelope: &CommandEnvelope) -> Result<CommandOutcome, CommandFailure> {
        let body = serde_json::to_value(envelope).map_err(|e| CommandFailure::Uncertain(e.to_string()))?;
        let kind = body.pointer("/command/kind").and_then(Value::as_str)
            .ok_or_else(|| CommandFailure::Uncertain("serialized command has no kind".into()))?;
        let (method, path) = crate::mcp::resource_target(kind, &body["command"])
            .map_err(|e| CommandFailure::Rejected(DomainError::new("invalid_input", e.to_string())))?;
        let mut resource_body = body["command"].clone();
        let command = resource_body.as_object_mut()
            .ok_or_else(|| CommandFailure::Uncertain("serialized command is not an object".into()))?;
        command.remove("kind");
        for key in ["data_epoch", "command_id", "expected_revision"] {
            command.insert(key.into(), body[key].clone());
        }
        let (status, value) = self.exchange(method, &path, Some(&resource_body)).map_err(|e| CommandFailure::Uncertain(format!("{e:#}")))?;
        classify(status, value)
    }

    /// A stored file. An attachment's copy comes whole from `/files` on this Mac and in
    /// verified chunks through the service; a document revision always in chunks.
    /// A card attachment's route is its stored reference, relative to the daemon's HOME.
    pub fn content(&self, route: &str) -> Result<Vec<u8>> {
        let stored;
        let route = if route.starts_with("files/") {
            stored = format!("{FILES}?path={}", crate::mcp::urlencode(route));
            &stored
        } else {
            route
        };
        match route.strip_prefix(FILES) {
            Some(query) if self.service.is_some() => content_via(&format!("{FILES}/chunk{query}"), |path| self.get::<Value>(path)),
            Some(_) => self.raw("GET", route, &[]),
            None => content_via(route, |path| self.get::<Value>(path)),
        }
    }

    /// A PR diff has a JSON text contract, unlike chunked attachment content.
    pub fn pull_request_diff(&self, id: &str, expected_head: &str) -> Result<PullRequestDiff> {
        pull_request_diff_via(id, expected_head, |path| self.get::<Value>(path))
    }

    /// Marks a file of the PR as viewed, or not, in GitHub, for the head the reader saw.
    pub fn set_file_viewed(&self, id: &str, head: &str, path: &str, viewed: bool) -> Result<FileViewed> {
        set_file_viewed_via(id, head, path, viewed, |route, body| self.post(route, body))
    }

    /// Copies a file into the daemon's storage and returns the reference commands attach:
    /// in one request on this Mac, in chunks through the service.
    pub fn upload(&self, name: &str, bytes: &[u8]) -> Result<AttachmentRef> {
        if self.service.is_none() {
            let answer = self.raw("POST", &format!("{UPLOADS}?name={}", crate::mcp::urlencode(name)), bytes)?;
            return Ok(serde_json::from_slice(&answer)?);
        }
        let finished = crate::service_client::Profile::upload_via(name, bytes, self.timeout.max(Duration::from_secs(120)), |step, body, _| self.post(&format!("{SERVICE_UPLOADS}/{step}"), &body))?;
        Ok(serde_json::from_value(finished.get("attachment").cloned().context("the upload returned no attachment")?)?)
    }

    /// A signed local request whose body, sent or answered, is the file itself.
    fn raw(&self, method: &str, path: &str, body: &[u8]) -> Result<Vec<u8>> {
        let (status, bytes) = self.local(method, path, "application/octet-stream", body)?;
        if status >= 400 {
            return Err(failure(status, &serde_json::from_slice(&bytes).unwrap_or(Value::Null)));
        }
        Ok(bytes)
    }
}

fn lang_name(lang: crate::i18n::Lang) -> &'static str {
    match lang { crate::i18n::Lang::En => "en", crate::i18n::Lang::Es => "es" }
}

pub const UPLOADS: &str = "/uploads";
pub const SERVICE_UPLOADS: &str = "/uploads";
pub const FILES: &str = "/files";
pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

/// Where an attachment's stored copy is read.
pub fn attachment_content(a: &AttachmentRef) -> String {
    format!("{FILES}?path={}", crate::mcp::urlencode(&a.content_reference))
}

pub fn document_content(revision: &str) -> String {
    format!("/document-revisions/{}/content", crate::mcp::urlencode(revision))
}

fn pull_request_diff_via(id: &str, expected_head: &str, mut get: impl FnMut(&str) -> Result<Value>) -> Result<PullRequestDiff> {
    if !alondra_core::views::id::valid(id) {
        return Err(anyhow!("invalid pull request ID"));
    }
    let route = format!("/pull-requests/{}/diff", alondra_core::views::id::path_segment(id));
    let response = get(&route)?;
    if response["pr_id"].as_str() != Some(id) {
        return Err(anyhow!("the pull request diff response has the wrong PR ID"));
    }
    if expected_head.is_empty() || response["head"].as_str() != Some(expected_head) {
        return Err(anyhow!("the pull request diff head changed"));
    }
    Ok(PullRequestDiff {
        text: response["diff"].as_str().context("the pull request diff response has no diff text")?.to_owned(),
        truncated: response["truncated"].as_bool().context("the pull request diff response has no truncation flag")?,
        files: response.get("files").and_then(|files| serde_json::from_value(files.clone()).ok()),
    })
}

fn set_file_viewed_via(id: &str, head: &str, path: &str, viewed: bool, post: impl FnOnce(&str, &Value) -> Result<Value>) -> Result<FileViewed> {
    if !alondra_core::views::id::valid(id) {
        return Err(anyhow!("invalid pull request ID"));
    }
    let route = format!("/pull-requests/{}/viewed-files", alondra_core::views::id::path_segment(id));
    let response = post(&route, &serde_json::json!({"path": path, "viewed": viewed, "head": head}))?;
    if response["pr_id"].as_str() != Some(id) || response["head"].as_str() != Some(head) || response["path"].as_str() != Some(path) {
        return Err(anyhow!("the viewed mark response is for another file"));
    }
    serde_json::from_value(response["viewed"].clone()).context("the viewed mark response has no mark")
}

/// Reads `route` from offset 0 to the end, refusing a file that changes between chunks.
fn content_via(route: &str, mut get: impl FnMut(&str) -> Result<Value>) -> Result<Vec<u8>> {
    use base64::Engine as _;
    let mut bytes = Vec::new();
    let mut version: Option<String> = None;
    let sep = if route.contains('?') { '&' } else { '?' };
    let limit = if route.starts_with("/interactions/") { alondra_core::domain::MAX_ATTACHMENT_BYTES } else { MAX_ATTACHMENT_BYTES };
    loop {
        let path = match &version {
            Some(v) => format!("{route}{sep}offset={}&version={}", bytes.len(), crate::mcp::urlencode(v)),
            None => format!("{route}{sep}offset=0"),
        };
        let chunk = get(&path)?;
        let v = chunk["version"].as_str().context("content version missing")?.to_string();
        anyhow::ensure!(version.as_ref().is_none_or(|old| *old == v) && chunk["offset"].as_u64() == Some(bytes.len() as u64), "the file changed while it was read");
        let total = chunk["total"].as_u64().context("content size missing")?;
        anyhow::ensure!(total <= limit, "the file is larger than {} MB", limit / (1024 * 1024));
        let data = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(chunk["data"].as_str().context("content data missing")?)?;
        bytes.extend_from_slice(&data);
        anyhow::ensure!(bytes.len() as u64 <= total && chunk["next_offset"].as_u64() == Some(bytes.len() as u64), "invalid content chunk");
        version = Some(v);
        if chunk["eof"] == true {
            anyhow::ensure!(bytes.len() as u64 == total, "incomplete file");
            return Ok(bytes);
        }
        anyhow::ensure!(!data.is_empty(), "the file stopped before its end");
    }
}

fn answer(mut resp: ureq::http::Response<ureq::Body>) -> Result<Value> {
    let status = resp.status().as_u16();
    let value = serde_json::from_str(&resp.body_mut().read_to_string().unwrap_or_default()).unwrap_or(Value::Null);
    if status >= 400 {
        return Err(failure(status, &value));
    }
    Ok(value)
}

/// A daemon older than this client may refuse a command only because it does not know it yet, as
/// in every upgrade until the daemon restarts. Being refused access says nothing about that.
fn refused_for_being_behind(status: u16, daemon: &str) -> bool {
    (400..500).contains(&status) && !matches!(status, 401 | 403) && alondra_core::update::newer(env!("CARGO_PKG_VERSION"), daemon)
}

/// A refusal from a daemon older than this client: nothing was written.
#[derive(Debug)]
pub struct DaemonBehind {
    pub daemon: String,
    pub refusal: anyhow::Error,
}

impl std::fmt::Display for DaemonBehind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.refusal.fmt(f)
    }
}

impl std::error::Error for DaemonBehind {}

/// alondrad's typed no: nothing was written. It reads as `refusal` to the user.
#[derive(Debug)]
pub struct Refused(pub DomainError);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&refusal(&self.0))
    }
}

impl std::error::Error for Refused {}

/// alondrad answered 404 without a domain error: it does not serve that route yet.
#[derive(Debug)]
pub struct Missing;

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(tr!("this alondrad does not have it yet", "este alondrad todavía no lo tiene"))
    }
}

impl std::error::Error for Missing {}

pub fn missing(error: &anyhow::Error) -> bool {
    error.is::<Missing>()
}

/// Why alondrad said no, in the user's language where the reason is a domain error it names.
fn failure(status: u16, body: &Value) -> anyhow::Error {
    if status == 404 && (body.is_null() || body.as_object().is_some_and(|o| o.is_empty())) {
        return anyhow::Error::new(Missing);
    }
    if body["code"] == "incompatible_protocol" {
        let service = serde_json::from_value(body["service"].clone()).ok();
        return anyhow::Error::new(Incompatible { service });
    }
    match alondra_core::views::action_outcome::Pending::of(body) {
        Some(alondra_core::views::action_outcome::Pending::InProgress) => return anyhow::Error::new(Running),
        Some(alondra_core::views::action_outcome::Pending::Unknown(reason)) => return anyhow::Error::new(OutcomeUnknown(reason)),
        None => {}
    }
    if let Ok(error) = serde_json::from_value::<DomainError>(body.clone()) {
        return anyhow::Error::new(Refused(error));
    }
    let message = match body {
        Value::Null => String::new(),
        _ => body.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| body.to_string()),
    };
    anyhow::Error::new(Answered { status, message })
}

/// The daemon no longer (or not yet) serves the views protocol this terminal speaks; a daemon
/// that announces itself says its version and range.
#[derive(Debug)]
pub struct Incompatible {
    pub service: Option<alondra_core::views::Service>,
}

impl std::fmt::Display for Incompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let version = self.service.as_ref().map_or("?", |s| s.version.as_str());
        f.write_str(&trf!(
            "Alondra was updated ({version}): restart the TUI · R",
            "Alondra se ha actualizado ({version}): reinicia la TUI · R"
        ))
    }
}

impl std::error::Error for Incompatible {}

/// The newest protocol this terminal speaks inside the range the daemon serves.
pub fn pick_protocol(serves: alondra_core::views::ProtocolRange) -> Option<u32> {
    let top = VIEWS_PROTOCOL.min(serves.max);
    (top >= alondra_core::views::VIEWS_PROTOCOL_MIN.max(serves.min)).then_some(top)
}

/// alondrad was reached and answered with an error of its own.
#[derive(Debug)]
pub struct Answered {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for Answered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (status, msg) = (self.status, &self.message);
        f.write_str(&trf!("alondrad answered {status}: {msg}", "alondrad respondió {status}: {msg}"))
    }
}

impl std::error::Error for Answered {}

/// alondrad is still running this action: sent again later, it answers with how it ended.
#[derive(Debug)]
pub struct Running;

impl std::fmt::Display for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(tr!("your Mac is still doing it", "tu Mac todavía lo está haciendo"))
    }
}

impl std::error::Error for Running {}

/// alondrad cannot tell whether this action happened, and says why.
#[derive(Debug)]
pub struct OutcomeUnknown(pub alondra_core::views::action_outcome::UnknownReason);

impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let outcome = alondra_core::views::unknown_outcome(self.0, crate::i18n::lang());
        write!(f, "{} {}", outcome.title, outcome.body.unwrap_or_default())
    }
}

impl std::error::Error for OutcomeUnknown {}

/// The request left and no answer came back: it may have run.
#[derive(Debug)]
pub struct Unanswered(pub String);

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unanswered {}

/// Nothing left this machine: the daemon could not be reached.
#[derive(Debug)]
pub struct Unsent(pub String);

impl std::fmt::Display for Unsent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsent {}

/// Whether a failed request could have reached the daemon, which decides what the user is told.
#[derive(Debug, PartialEq)]
pub enum Delivery {
    Unsent,
    Unanswered,
    /// The daemon is still running it: asked again, it answers how it ended.
    Running,
    /// The daemon cannot tell whether it happened.
    Unknown(alondra_core::views::action_outcome::UnknownReason),
    /// The daemon answered; its words, when it gave any.
    Answered(Option<String>),
}

pub fn delivery(error: &anyhow::Error) -> Delivery {
    for cause in error.chain() {
        if let Some(refused) = cause.downcast_ref::<Refused>() {
            return Delivery::Answered(Some(refusal(&refused.0)));
        }
        if cause.is::<Running>() {
            return Delivery::Running;
        }
        if let Some(unknown) = cause.downcast_ref::<OutcomeUnknown>() {
            return Delivery::Unknown(unknown.0);
        }
        if let Some(answered) = cause.downcast_ref::<Answered>() {
            return Delivery::Answered(Some(answered.message.clone()).filter(|message| !message.is_empty()));
        }
        if cause.is::<Missing>() {
            return Delivery::Answered(None);
        }
        if cause.is::<Unsent>() {
            return Delivery::Unsent;
        }
        if cause.is::<Unanswered>() {
            return Delivery::Unanswered;
        }
        if let Some(error) = cause.downcast_ref::<ureq::Error>() {
            return if never_left(error) { Delivery::Unsent } else { Delivery::Unanswered };
        }
    }
    Delivery::Unanswered
}

fn never_left(error: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    match error {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::BadUri(_) => true,
        ureq::Error::Timeout(timeout) => matches!(timeout, ureq::Timeout::Resolve | ureq::Timeout::Connect),
        ureq::Error::Io(io) => matches!(
            io.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::NotFound | ErrorKind::AddrNotAvailable | ErrorKind::PermissionDenied
        ),
        _ => false,
    }
}

/// How a domain command ended when it did not succeed.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandFailure {
    /// The daemon refused it: nothing was written.
    Rejected(DomainError),
    /// The request left but no answer says whether it was applied.
    Uncertain(String),
}

fn classify(status: u16, value: Value) -> Result<CommandOutcome, CommandFailure> {
    if status < 300 {
        return serde_json::from_value(value).map_err(|e| CommandFailure::Uncertain(e.to_string()));
    }
    match serde_json::from_value::<DomainError>(value) {
        Ok(error) if status < 500 || error.code == "storage_unavailable" => Err(CommandFailure::Rejected(error)),
        Ok(error) => Err(CommandFailure::Uncertain(error.to_string())),
        Err(_) => Err(CommandFailure::Uncertain(trf!("alondrad answered {status}", "alondrad respondió {status}"))),
    }
}

/// A fresh identifier for a command or for an entity the command creates.
pub fn new_id(prefix: &str) -> String {
    let bytes = crate::service_client::random::<16>().unwrap_or_default();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{hex}")
}

/// Views actions require a UUID v4 so the daemon can deduplicate retries.
fn new_views_command_id() -> String {
    let mut bytes = crate::service_client::random::<16>().expect("secure command randomness");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

pub fn envelope(epoch: &DataEpoch, expected_revision: Revision, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: epoch.clone(), command_id: new_id("tui").into(), expected_revision, command }
}

/// What a refusal means to the user; the code decides, the daemon's English message is the fallback.
pub fn refusal(error: &DomainError) -> String {
    match error.code.as_str() {
        "stale_revision" | "stale_epoch" => tr!("It changed since you saw it: check it and try again", "Ha cambiado desde que lo viste: revísalo y vuelve a intentarlo").into(),
        "not_found" => tr!("Alondra no longer has it", "Alondra ya no lo tiene").into(),
        "task_not_active" => tr!("The task is no longer active", "La tarea ya no está activa").into(),
        "agent_busy" => tr!("That node is on another task until it ends: launch a new node for this one", "Ese nodo está en otra tarea hasta que acabe: lanza un nodo nuevo para esta").into(),
        "agent_used" => tr!("That node already worked on another task: launch a new node for this one", "Ese nodo ya trabajó en otra tarea: lanza un nodo nuevo para esta").into(),
        "task_closing" => tr!("This task's pull request is already merged and the task is closing: ask for more work as a new request", "La PR de esta tarea ya está mergeada y la tarea se está cerrando: pide más trabajo como una petición nueva").into(),
        "incompatible_protocol" => tr!("This alondrad does not accept this domain request", "Este alondrad no acepta esta petición del dominio").into(),
        _ => {
            let (code, why) = (&error.code, &error.message);
            trf!("{why} ({code})", "{why} ({code})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_for_this_thread};
    use serde_json::json;

    #[test]
    fn spanish_says_merge_for_a_pull_request() {
        let found: Vec<&str> = include_str!("client.rs").lines()
            .filter(|line| [concat!("fu", "sion"), "fusi\u{f3}n"].iter().any(|word| line.to_lowercase().contains(word)))
            .collect();
        assert!(found.is_empty(), "in Spanish a PR gets a merge: {found:#?}");
    }

    #[test]
    fn a_refusal_is_typed_and_a_lost_answer_is_uncertain() {
        let stale = classify(409, json!({"code": "stale_revision", "message": "Refresh", "current_revision": 7, "subject": null}));
        assert!(matches!(stale, Err(CommandFailure::Rejected(e)) if e.code == "stale_revision" && e.current_revision == Some(7)));
        assert!(matches!(classify(502, json!({"error": "bad gateway"})), Err(CommandFailure::Uncertain(_))));
        assert!(matches!(classify(500, json!({"code": "storage_unavailable", "message": "x", "current_revision": null, "subject": null})), Err(CommandFailure::Rejected(_))));
        let ok = classify(200, json!({"revision": 8, "changed": true, "effects": [], "created_ids": []}));
        assert!(matches!(ok, Ok(o) if o.revision == 8));
    }

    #[test]
    fn a_refusal_reads_in_the_users_language() {
        set_for_this_thread(Lang::Es);
        let said = |code: &str| refusal(&DomainError::new(code, "english detail"));
        assert_eq!(said("stale_revision"), "Ha cambiado desde que lo viste: revísalo y vuelve a intentarlo");
        assert_eq!(said("not_found"), "Alondra ya no lo tiene");
        assert_eq!(said("something_new"), "english detail (something_new)");
        assert_eq!(failure(409, &json!({"code": "not_found", "message": "gone", "current_revision": null, "subject": null})).to_string(), "Alondra ya no lo tiene");
    }

    #[test]
    fn a_stored_file_is_read_chunk_by_chunk_and_a_change_between_chunks_stops_it() {
        use base64::Engine as _;
        let data = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        let mut asked = vec![];
        let bytes = content_via("/attachments/att-1/content", |path| {
            asked.push(path.to_string());
            Ok(if path.ends_with("offset=0") {
                json!({"version": "v-1", "data": data("ab"), "offset": 0, "total": 3, "next_offset": 2, "eof": false})
            } else {
                json!({"version": "v-1", "data": data("c"), "offset": 2, "total": 3, "next_offset": 3, "eof": true})
            })
        })
        .unwrap();
        assert_eq!(bytes, b"abc");
        assert_eq!(asked, ["/attachments/att-1/content?offset=0", "/attachments/att-1/content?offset=2&version=v-1"]);
        let changed = content_via("/r", |path| {
            Ok(if path.ends_with("offset=0") { json!({"version": "v-1", "data": data("ab"), "offset": 0, "total": 3, "next_offset": 2, "eof": false}) } else { json!({"version": "v-2", "data": data("c"), "offset": 2, "total": 3, "next_offset": 3, "eof": true}) })
        });
        assert!(changed.unwrap_err().to_string().contains("changed"));
        let contract = crate::runtime::fixture("requests.json")["attachment_content"]["response"].clone();
        assert_eq!(content_via("/attachments/att-1/content", |_| Ok(contract.clone())).unwrap(), b"abc");
    }

    #[test]
    fn a_card_attachment_may_be_as_large_as_the_daemon_allows_and_a_stored_file_stays_at_20_mb() {
        use base64::Engine as _;
        let first = |total: u64| move |path: &str| {
            if path.ends_with("offset=0") {
                Ok(json!({"version": "v-1", "data": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("ab"), "offset": 0, "total": total, "next_offset": 2, "eof": false}))
            } else {
                Err(anyhow!("second chunk asked"))
            }
        };
        let video = "/interactions/i-1/attachments/clip.mp4/content";
        let thirty = 30 * 1024 * 1024;
        assert_eq!(content_via(video, first(thirty)).unwrap_err().to_string(), "second chunk asked");
        assert_eq!(content_via("/files/chunk?path=a", first(thirty)).unwrap_err().to_string(), "the file is larger than 20 MB");
        assert_eq!(content_via(video, first(alondra_core::domain::MAX_ATTACHMENT_BYTES + 1)).unwrap_err().to_string(), "the file is larger than 64 MB");
    }

    #[test]
    fn a_pr_diff_reads_the_viewed_marks_and_a_mark_is_set_for_the_head_seen() {
        let id = "github.com/bitomule/alondra#154";
        let answer = |files: Value| move |_: &str| Ok(json!({"pr_id": id, "head": "abc", "diff": "d", "truncated": false, "files": files}));
        let marked = pull_request_diff_via(id, "abc", answer(json!([{"path": "a", "viewed": "viewed"}, {"path": "b", "viewed": "changed"}]))).unwrap();
        assert_eq!(marked.files, Some(vec![
            FileMark { path: "a".into(), viewed: FileViewed::Viewed },
            FileMark { path: "b".into(), viewed: FileViewed::Changed },
        ]));
        let unknown = pull_request_diff_via(id, "abc", answer(json!([{"path": "a", "viewed": "someday"}]))).unwrap();
        assert_eq!((unknown.files, unknown.text.as_str()), (None, "d"), "a mark it cannot read never costs the diff");
        let mut sent = Vec::new();
        let viewed = set_file_viewed_via(id, "abc", "src/a.rs", true, |route, body| {
            sent.push((route.to_owned(), body.clone()));
            Ok(json!({"pr_id": id, "head": "abc", "path": "src/a.rs", "viewed": "viewed"}))
        }).unwrap();
        assert_eq!(viewed, FileViewed::Viewed);
        assert_eq!(sent, [("/pull-requests/github.com%2Fbitomule%2Falondra%23154/viewed-files".to_owned(), json!({"path": "src/a.rs", "viewed": true, "head": "abc"}))]);
        assert!(set_file_viewed_via(id, "abc", "src/a.rs", true, |_, _| Ok(json!({"pr_id": id, "head": "abc", "path": "src/b.rs", "viewed": "viewed"}))).is_err());
        assert!(set_file_viewed_via("../secret", "abc", "a", true, |_, _| panic!("invalid ID reached transport")).is_err());
    }

    #[test]
    fn a_canonical_pr_diff_uses_the_json_contract_and_one_encoded_path_segment() {
        let mut paths = Vec::new();
        let diff = pull_request_diff_via("github.com/bitomule/alondra#154", "abc", |path| {
            paths.push(path.to_owned());
            Ok(json!({"pr_id":"github.com/bitomule/alondra#154", "head":"abc", "diff":"diff --git a/a b/a\n+x", "truncated":false}))
        }).unwrap();
        assert_eq!(diff.text, "diff --git a/a b/a\n+x");
        assert!(!diff.truncated);
        assert_eq!(paths, ["/pull-requests/github.com%2Fbitomule%2Falondra%23154/diff"]);
        assert!(pull_request_diff_via("../secret", "abc", |_| panic!("invalid ID reached transport")).is_err());
        assert!(pull_request_diff_via("github.com/bitomule/alondra#154", "abc", |_| Ok(json!({"pr_id":"github.com/bitomule/alondra#154", "version":"v1","data":"abc"}))).is_err());
        assert!(pull_request_diff_via("github.com/bitomule/alondra#154", "abc", |_| Ok(json!({"pr_id":"github.com/else/repo#154", "head":"abc", "diff":"wrong PR", "truncated":false}))).is_err());
        assert!(pull_request_diff_via("github.com/bitomule/alondra#154", "abc", |_| Ok(json!({"pr_id":"github.com/bitomule/alondra#154", "head":"changed", "diff":"stale", "truncated":false}))).is_err());
        let cut = pull_request_diff_via("github.com/bitomule/alondra#154", "abc", |_| Ok(json!({"pr_id":"github.com/bitomule/alondra#154", "head":"abc", "diff":"partial", "truncated":true}))).unwrap();
        assert!(cut.truncated);
        assert_eq!(diff.files, None);
    }

    #[test]
    fn a_route_the_daemon_lacks_reads_as_missing_not_as_a_refusal() {
        assert!(missing(&failure(404, &Value::Null)));
        assert!(!missing(&failure(404, &json!({"code": "not_found", "message": "gone", "current_revision": null, "subject": null}))));
    }

    #[test]
    fn every_command_gets_its_own_identifier() {
        let epoch = DataEpoch::from("e");
        let a = envelope(&epoch, 3, Command::MarkRead { interaction_id: "i".into() });
        let b = envelope(&epoch, 3, Command::MarkRead { interaction_id: "i".into() });
        assert_ne!(a.command_id, b.command_id);
        assert_eq!((a.protocol_version, a.expected_revision), (PROTOCOL_VERSION, 3));
    }

    #[test]
    fn views_action_uses_uuid_v4_command_id() {
        let a = Client::action_body("e", "read:hf-weekly-2026-09-30@1", None, serde_json::json!({}));
        let b = Client::action_body("e", "read:hf-weekly-2026-09-30@1", None, serde_json::json!({}));
        let id = a["command_id"].as_str().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(a["command_id"], b["command_id"]);
    }
}

#[cfg(test)]
mod signing_tests {
    use super::*;
    use p256::ecdsa::signature::Verifier as _;
    use sha2::Digest as _;

    fn signer() -> Signer {
        Signer { device: "dev".into(), key: p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap() }
    }

    #[test]
    fn identical_requests_signed_back_to_back_never_repeat() {
        use base64::Engine as _;
        let signer = signer();
        let verifying = *signer.key.verifying_key();
        let mut timestamps = std::collections::HashSet::new();
        let mut signatures = std::collections::HashSet::new();
        for _ in 0..200 {
            let headers = signer.sign("GET", "/views/home", b"").unwrap();
            let timestamp = headers[1].1.clone();
            let signature = headers[2].1.clone();
            let der = base64::engine::general_purpose::STANDARD.decode(&signature).unwrap();
            let message = format!("GET\n/views/home\n{timestamp}\n{}", sha2::Sha256::digest(b"").iter().map(|b| format!("{b:02x}")).collect::<String>());
            verifying.verify(message.as_bytes(), &p256::ecdsa::Signature::from_der(&der).unwrap()).unwrap();
            assert!(timestamps.insert(timestamp));
            assert!(signatures.insert(signature));
        }
    }

    #[test]
    fn independent_signers_at_the_same_timestamp_do_not_repeat_signatures() {
        use base64::Engine as _;
        let first = signer();
        let second = signer();
        let message = format!("GET\n/views/home\n1000\n{}", sha2::Sha256::digest(b"").iter()
            .map(|byte| format!("{byte:02x}")).collect::<String>());
        let mut signatures = std::collections::HashSet::new();
        for source in [&first, &second].into_iter().cycle().take(32) {
            let headers = source.sign_at("GET", "/views/home", b"", 1000).unwrap();
            let encoded = &headers[2].1;
            let der = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
            source.key.verifying_key().verify(message.as_bytes(),
                &p256::ecdsa::Signature::from_der(&der).unwrap()).unwrap();
            assert!(signatures.insert(encoded.clone()), "independent requests must not look like captured replays");
        }
    }

    #[test]
    fn entropy_failure_never_produces_unsigned_or_deterministic_fallback_headers() {
        struct FailedEntropy;
        impl p256::elliptic_curve::rand_core::TryRng for FailedEntropy {
            type Error = std::io::Error;
            fn try_next_u32(&mut self) -> std::io::Result<u32> { Err(std::io::Error::other("synthetic")) }
            fn try_next_u64(&mut self) -> std::io::Result<u64> { Err(std::io::Error::other("synthetic")) }
            fn try_fill_bytes(&mut self, _: &mut [u8]) -> std::io::Result<()> { Err(std::io::Error::other("synthetic")) }
        }
        impl p256::elliptic_curve::rand_core::TryCryptoRng for FailedEntropy {}
        assert!(signer().sign_with_entropy("GET", "/views/home", b"", 1000, &mut FailedEntropy).is_err());
    }

    #[test]
    fn timestamps_increase_across_threads() {
        let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(|| (0..500).map(|_| unique_timestamp()).collect::<Vec<_>>())).collect();
        let mut all: Vec<u64> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total);
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A synthetic daemon that answers scripted paths, one request per connection.
    fn peer(script: Vec<(&'static str, Value)>) -> (Client, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let log = seen.clone();
        let server = std::thread::spawn(move || {
            for (expected, body) in script {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                log.lock().unwrap().push(path.clone());
                assert_eq!(path, expected);
                let body = serde_json::to_vec(&body).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (Client::at(base), seen, server)
    }

    /// A daemon that takes the request and then answers with `reply`, or never answers.
    fn one_shot(reply: Option<&'static str>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut chunk = [0; 4096];
            let _ = stream.read(&mut chunk);
            match reply {
                Some(reply) => stream.write_all(reply.as_bytes()).unwrap(),
                None => std::thread::sleep(Duration::from_millis(800)),
            }
        });
        (base, server)
    }

    fn post_to(base: String, timeout: Duration) -> anyhow::Error {
        let mut client = Client::new(timeout);
        client.base = base;
        client.post("/actions", &serde_json::json!({})).unwrap_err()
    }

    #[test]
    fn a_project_name_with_a_space_and_an_accent_is_encoded_once_in_the_query() {
        let lang = lang_name(crate::i18n::lang());
        let path: &'static str = format!("/views/new_request?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}&project=Mi+proyecto+ni%C3%B1o").leak();
        let (client, seen, server) = peer(vec![(path, Value::Null)]);
        let _ = client.views(&format!("new_request?project={}", crate::mcp::urlencode("Mi proyecto niño")), None);
        let served = server.join();
        assert_eq!(*seen.lock().unwrap(), [path]);
        served.unwrap();
    }

    #[test]
    fn a_view_key_with_options_asks_for_its_path_with_the_options_in_the_query() {
        let lang = lang_name(crate::i18n::lang());
        let section = format!("/views/tasks?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}&section=finished&after=0");
        let card = format!("/views/card%3Aturn-ended-t-1?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}");
        let project = format!("/views/new_request?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}&project=bitomule%2Falondra");
        let expected: [&'static str; 3] = [section.leak(), card.leak(), project.leak()];
        let (client, seen, server) = peer(expected.iter().map(|path| (*path, Value::Null)).collect());
        let _ = client.views("tasks?section=finished&after=0", None);
        let _ = client.views("card:turn-ended-t-1", None);
        let _ = client.views(&format!("new_request?project={}", crate::mcp::urlencode("bitomule/alondra")), None);
        let served = server.join();
        assert_eq!(*seen.lock().unwrap(), expected);
        served.unwrap();
    }

    #[test]
    fn a_card_attachment_is_read_from_files_by_the_reference_the_daemon_stored() {
        let shot = AttachmentRef { id: "att-1".into(), name: "shot.png".into(), content_digest: "a".repeat(64), content_reference: "files/uploads/u-1/shot.png".into(), bytes: 4 };
        let (client, seen, server) = peer(vec![("/files?path=files%2Fuploads%2Fu-1%2Fshot.png", Value::from("png"))]);
        let bytes = client.content(&shot.content_reference).unwrap();
        server.join().unwrap();
        assert_eq!(*seen.lock().unwrap(), ["/files?path=files%2Fuploads%2Fu-1%2Fshot.png"]);
        assert_eq!(bytes, br#""png""#);
    }

    #[test]
    fn a_daemon_that_is_not_listening_got_nothing() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let error = post_to(format!("http://127.0.0.1:{port}"), Duration::from_secs(2));
        assert_eq!(delivery(&error), Delivery::Unsent, "{error:#}");
    }

    #[test]
    fn a_daemon_that_took_the_request_and_never_answered_may_have_run_it() {
        let (base, server) = one_shot(None);
        let error = post_to(base, Duration::from_millis(300));
        assert_eq!(delivery(&error), Delivery::Unanswered, "{error:#}");
        server.join().unwrap();
    }

    #[test]
    fn a_daemon_error_is_its_own_answer() {
        let (base, server) = one_shot(Some(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"error\":\"disk full\"}",
        ));
        let error = post_to(base, Duration::from_secs(2));
        assert_eq!(delivery(&error), Delivery::Answered(Some("disk full".into())), "{error:#}");
        server.join().unwrap();
    }

    #[test]
    fn still_running_and_unknown_outcomes_are_read_from_their_kind_never_shown_as_their_code() {
        use alondra_core::views::action_outcome::{UnknownReason, in_progress_body, unknown_body};
        assert_eq!(delivery(&failure(503, &in_progress_body())), Delivery::Running);
        assert_eq!(delivery(&failure(409, &unknown_body(UnknownReason::Interrupted))), Delivery::Unknown(UnknownReason::Interrupted));
        let older = serde_json::json!({"error": "outcome_unknown", "kind": "outcome_unknown"});
        assert_eq!(delivery(&failure(409, &older)), Delivery::Unknown(UnknownReason::Other));
    }

    #[test]
    fn a_direct_route_that_dropped_mid_request_is_not_nothing_sent() {
        let error = anyhow::Error::new(Unanswered("stopped".into())).context("posting");
        assert_eq!(delivery(&error), Delivery::Unanswered);
        let error = anyhow::Error::new(Unsent("not on this network".into()));
        assert_eq!(delivery(&error), Delivery::Unsent);
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use p256::ecdsa::signature::Verifier as _;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Condvar, Mutex};

    #[derive(Default)]
    struct Seen {
        pairings: usize,
        pairs: usize,
        devices: Vec<(String, String)>,
        entered: usize,
        released: bool,
    }

    /// alondrad's pairing and signature check over TCP. The first `/pairing` is answered only
    /// once `callers` have entered or a second `/pairing` arrives, so racing pairings meet.
    struct Daemon {
        base: String,
        state: Arc<(Mutex<Seen>, Condvar)>,
        server: Option<std::thread::JoinHandle<()>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Daemon {
        fn start(callers: usize, reject_every_signature: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let state = Arc::new((Mutex::new(Seen::default()), Condvar::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (shared, stopped) = (state.clone(), stop.clone());
            let server = std::thread::spawn(move || {
                let mut handlers = vec![];
                for stream in listener.incoming() {
                    if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let state = shared.clone();
                    handlers.push(std::thread::spawn(move || serve(stream.unwrap(), &state, callers, reject_every_signature)));
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            });
            Daemon { base, state, server: Some(server), stop }
        }

        fn enter(&self) {
            let (seen, changed) = &*self.state;
            seen.lock().unwrap().entered += 1;
            changed.notify_all();
        }

        fn add_device(&self, device: &str, key: &p256::ecdsa::SigningKey) {
            self.state.0.lock().unwrap().devices.push((device.into(), public(key)));
        }

        fn counts(&self) -> (usize, usize) {
            let seen = self.state.0.lock().unwrap();
            (seen.pairings, seen.pairs)
        }

        fn key_of(&self, device: &str) -> Option<String> {
            self.state.0.lock().unwrap().devices.iter().find(|(id, _)| id == device).map(|(_, key)| key.clone())
        }

        fn client(&self, pairing: &Arc<Pairing>) -> Client {
            let mut client = Client::at(self.base.clone());
            pairing.signer(&client).unwrap();
            client.pairing = Some(pairing.clone());
            client
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
            if let Some(server) = self.server.take() {
                server.join().unwrap();
            }
        }
    }

    fn public(key: &p256::ecdsa::SigningKey) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes())
    }

    fn serve(stream: TcpStream, state: &(Mutex<Seen>, Condvar), callers: usize, reject_every_signature: bool) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap().to_owned(), parts.next().unwrap().to_owned());
        let mut headers = std::collections::HashMap::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let Some((name, value)) = header.trim_end().split_once(": ") else { break };
            headers.insert(name.to_ascii_lowercase(), value.to_owned());
        }
        let mut body = vec![0; headers.get("content-length").map_or(0, |n| n.parse().unwrap())];
        reader.read_exact(&mut body).unwrap();
        let (seen, changed) = state;
        let (status, reply) = match path.as_str() {
            "/pairing" => {
                let mut seen = seen.lock().unwrap();
                seen.pairings += 1;
                changed.notify_all();
                if seen.pairings == 1 {
                    seen = changed.wait_while(seen, |s| !s.released && s.entered < callers && s.pairings < 2).unwrap();
                }
                seen.released = true;
                changed.notify_all();
                (200, serde_json::json!({"code": format!("code-{}", seen.pairings)}))
            }
            "/pair" => {
                let request: Value = serde_json::from_slice(&body).unwrap();
                let mut seen = seen.lock().unwrap();
                seen.pairs += 1;
                let device = format!("device-{}", seen.pairs);
                seen.devices.push((device.clone(), request["public_key"].as_str().unwrap().to_owned()));
                (200, serde_json::json!({"device_id": device}))
            }
            _ => match verify(&seen.lock().unwrap().devices, &headers, &method, &path, &body) {
                Err(reason) => (401, serde_json::json!({"error": reason})),
                Ok(()) if reject_every_signature => (401, serde_json::json!({"error": "wrong signature"})),
                Ok(()) => (200, serde_json::json!({"ok": true})),
            },
        };
        let reply = serde_json::to_vec(&reply).unwrap();
        let mut stream = stream;
        write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len()).unwrap();
        stream.write_all(&reply).unwrap();
    }

    fn verify(devices: &[(String, String)], headers: &std::collections::HashMap<String, String>, method: &str, path: &str, body: &[u8]) -> Result<(), &'static str> {
        use base64::Engine as _;
        use sha2::Digest as _;
        let device = headers.get("x-alondra-device").ok_or("missing X-Alondra-Device")?;
        let key = devices.iter().find(|(id, _)| id == device).map(|(_, key)| key).ok_or("device not paired")?;
        let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&base64::engine::general_purpose::STANDARD.decode(key).unwrap()).unwrap();
        let digest: String = sha2::Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
        let message = format!("{method}\n{path}\n{}\n{digest}", headers["x-alondra-timestamp"]);
        let der = base64::engine::general_purpose::STANDARD.decode(&headers["x-alondra-signature"]).unwrap();
        key.verify(message.as_bytes(), &p256::ecdsa::Signature::from_der(&der).unwrap()).map_err(|_| "wrong signature")
    }

    fn write_pair(home: &std::path::Path, device: &str, key: &p256::ecdsa::SigningKey) {
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        std::fs::write(home.join(KEY_FILE), key.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes()).unwrap();
        std::fs::write(home.join(DEVICE_FILE), device).unwrap();
    }

    fn new_home() -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn key(byte: u8) -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[byte; 32]).unwrap()
    }

    /// The pair on disk is one alondrad has: its device carries the stored key.
    fn assert_stored_pair_matches(daemon: &Daemon, pairing: &Pairing) -> Signer {
        let stored = pairing.stored().expect("a stored pair");
        assert_eq!(daemon.key_of(&stored.device), Some(public(&stored.key)), "key and device come from different pairings");
        stored
    }

    fn first_run(daemon: &Arc<Daemon>, pairings: Vec<Arc<Pairing>>) -> Vec<Signer> {
        let callers: Vec<_> = pairings.into_iter().map(|pairing| {
            let daemon = daemon.clone();
            std::thread::spawn(move || {
                daemon.enter();
                pairing.signer(&Client::at(daemon.base.clone())).unwrap()
            })
        }).collect();
        callers.into_iter().map(|caller| caller.join().unwrap()).collect()
    }

    #[test]
    fn threads_of_one_process_on_a_new_home_pair_once() {
        let home = new_home();
        let daemon = Arc::new(Daemon::start(6, false));
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let signers = first_run(&daemon, vec![pairing.clone(); 6]);
        assert_eq!(daemon.counts(), (1, 1));
        let stored = assert_stored_pair_matches(&daemon, &pairing);
        assert!(signers.iter().all(|signer| signer.same_pair(&stored)));
    }

    #[test]
    fn a_process_waiting_on_the_lock_reuses_the_pair_the_first_one_made() {
        let home = new_home();
        let daemon = Arc::new(Daemon::start(3, false));
        let processes: Vec<_> = (0..3).map(|_| Arc::new(Pairing::at(home.to_path_buf()))).collect();
        let signers = first_run(&daemon, processes.clone());
        assert_eq!(daemon.counts(), (1, 1));
        let stored = assert_stored_pair_matches(&daemon, &processes[0]);
        assert!(signers.iter().all(|signer| signer.same_pair(&stored)));
    }

    #[test]
    fn a_key_left_without_its_device_is_paired_again_whole() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        std::fs::write(home.join(KEY_FILE), b"left by a cut pairing").unwrap();
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        daemon.enter();
        daemon.client(&pairing);
        assert_eq!(daemon.counts(), (1, 1));
        assert_stored_pair_matches(&daemon, &pairing);
    }

    #[test]
    fn a_cut_between_the_two_files_leaves_no_device_to_sign_with() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        std::fs::write(home.join(DEVICE_FILE), "device-stale").unwrap();
        std::fs::create_dir(home.join(format!(".{DEVICE_FILE}.{}", std::process::id()))).unwrap();
        let pairing = Pairing::at(home.to_path_buf());
        daemon.enter();
        assert!(pairing.signer(&Client::at(daemon.base.clone())).is_err());
        assert!(pairing.stored().is_none(), "a new key was left beside the device of another pairing");
    }

    #[test]
    fn a_stored_key_alondrad_rejects_is_paired_again_once_and_the_request_goes_through() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        daemon.add_device("device-mixed", &key(1));
        write_pair(&home, "device-mixed", &key(2));
        daemon.enter();
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let client = daemon.client(&pairing);
        assert_eq!(client.get::<Value>("/views/home").unwrap()["ok"], true);
        assert_eq!(daemon.counts(), (1, 1));
        assert_stored_pair_matches(&daemon, &pairing);
        assert_eq!(client.get::<Value>("/views/home").unwrap()["ok"], true);
        assert_eq!(daemon.counts(), (1, 1));
    }

    #[test]
    fn a_process_pairs_again_at_most_once() {
        let home = new_home();
        let daemon = Daemon::start(1, true);
        daemon.add_device("device-mixed", &key(1));
        write_pair(&home, "device-mixed", &key(1));
        daemon.enter();
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let client = daemon.client(&pairing);
        assert!(client.get::<Value>("/views/home").is_err());
        assert!(client.get::<Value>("/views/home").is_err());
        assert_eq!(daemon.counts(), (1, 1));
    }

    #[test]
    fn an_unpaired_device_is_not_paired_again() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        write_pair(&home, "device-revoked", &key(1));
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let client = daemon.client(&pairing);
        assert!(format!("{:#}", client.get::<Value>("/views/home").unwrap_err()).contains("device not paired"));
        assert_eq!(daemon.counts(), (0, 0));
        assert_eq!(pairing.stored().unwrap().device, "device-revoked");
    }
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn mcp_operation_header_is_scoped_to_one_client_clone() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut present = vec![];
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut bytes = vec![];
                while !bytes.ends_with(b"\r\n\r\n") { let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); bytes.push(byte[0]); }
                present.push(String::from_utf8(bytes).unwrap().to_ascii_lowercase().contains("x-alondra-mcp-operation: abcd0123\r\n"));
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
            }
            present
        });
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into();
        let client = Client { base, agent, pairing: None, service: None, direct: None, timeout: Duration::from_secs(5), protocol: VIEWS_PROTOCOL, mcp_operation: None, capabilities: Default::default(), response_observer: None };
        let scoped = client.with_mcp_operation("abcd0123");
        assert_eq!(client.operation_id(), None); assert_eq!(scoped.operation_id(), Some("abcd0123"));
        let _: Value = client.get("/echo").unwrap(); let _: Value = scoped.get("/echo").unwrap(); let _: Value = client.get("/echo").unwrap();
        assert_eq!(server.join().unwrap(), [false, true, false]);
    }

    #[test]
    fn a_native_plan_mode_post_carries_its_scoped_mcp_operation_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut observed = vec![];
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut bytes = vec![];
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
                assert!(headers.starts_with("post /agents/plan-mode http/1.1\r\n"));
                assert!(headers.contains("authorization: bearer fixture-capability\r\n"));
                let length = headers.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse::<usize>().unwrap();
                let mut body = vec![0; length]; stream.read_exact(&mut body).unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), serde_json::json!({"task_id":"fixture-task"}));
                let present = headers.contains("x-alondra-mcp-operation: abcd0123\r\n");
                observed.push(present);
                let (status, body) = if present {
                    ("200 OK", r#"{"pending":true}"#)
                } else {
                    ("400 Bad Request", r#"{"code":"incompatible_protocol","message":"A native operation identity is required"}"#)
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            observed
        });
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into();
        let client = Client { base, agent, pairing: None, service: None, direct: None, timeout: Duration::from_secs(5), protocol: VIEWS_PROTOCOL, mcp_operation: None, capabilities: Default::default(), response_observer: None };
        let scoped = client.with_mcp_operation("abcd0123");
        let body = serde_json::json!({"task_id":"fixture-task"});
        let first = client.domain_post("/agents/plan-mode", "fixture-capability", &body);
        let native = scoped.domain_post("/agents/plan-mode", "fixture-capability", &body);
        let last = client.domain_post("/agents/plan-mode", "fixture-capability", &body);
        assert_eq!(server.join().unwrap(), [false, true, false]);
        assert!(first.is_err());
        assert_eq!(native.unwrap()["pending"], true);
        assert!(last.is_err());
    }
}
