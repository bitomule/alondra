use std::sync::OnceLock;
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::domain::reads::{Item, TextChunk};
use alondra_core::domain::{AttachmentRef, Command, CommandEnvelope, CommandOutcome, DataEpoch, DomainError, Interaction, InteractionId, PROTOCOL_VERSION, Revision};
use alondra_core::views::{View as ViewDocument, VIEWS_PROTOCOL};
use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::Value;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The effective configuration; a broken `config.toml` falls back to defaults here and is
/// reported by `alondra doctor`.
pub fn config() -> &'static Config {
    CONFIG.get_or_init(|| Config::load().unwrap_or_else(|_| Config::defaults()))
}

pub struct Client {
    base: String,
    agent: ureq::Agent,
    signer: Option<Signer>,
    service: Option<std::sync::Arc<crate::service_client::Profile>>,
    timeout: Duration,
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
}

pub struct PullRequestDiff {
    pub text: String,
    pub truncated: bool,
}

struct Signer {
    device: String,
    key: p256::ecdsa::SigningKey,
}

static LAST_TIMESTAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Signatures are deterministic, so two identical requests signed in the same millisecond
/// would be byte-identical and alondrad would refuse the second as a replay.
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

impl Signer {
    fn sign(&self, method: &str, path: &str, body: &[u8]) -> [(&'static str, String); 3] {
        use base64::Engine as _;
        use p256::ecdsa::signature::Signer as _;
        use sha2::Digest as _;
        let timestamp = unique_timestamp().to_string();
        let digest: String = sha2::Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
        let message = format!("{method}\n{path}\n{timestamp}\n{digest}");
        let signature: p256::ecdsa::Signature = self.key.sign(message.as_bytes());
        [
            ("x-alondra-device", self.device.clone()),
            ("x-alondra-timestamp", timestamp),
            ("x-alondra-signature", base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes())),
        ]
    }

    /// The key this terminal signs the user's answers with, paired with alondrad on first use.
    fn load_or_pair(unsigned: &Client) -> Result<Signer> {
        use base64::Engine as _;
        use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
        let home = &config().home;
        let key_path = home.join(KEY_FILE);
        let device_path = home.join(DEVICE_FILE);
        if let (Ok(pem), Ok(device)) = (std::fs::read_to_string(&key_path), std::fs::read_to_string(&device_path)) {
            let key = p256::ecdsa::SigningKey::from_pkcs8_pem(&pem).map_err(|e| anyhow!("{KEY_FILE}: {e}"))?;
            return Ok(Signer { device: device.trim().to_string(), key });
        }
        let mut seed = [0u8; 32];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut seed)?;
        let key = p256::ecdsa::SigningKey::from_slice(&seed).map_err(|e| anyhow!("key: {e}"))?;
        let code = unsigned.post("/pairing", &serde_json::json!({}))?.get("code").and_then(Value::as_str).map(str::to_string).context("no pairing code")?;
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes());
        let host = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "Mac".into());
        let paired = unsigned.post("/pair", &serde_json::json!({"code": code, "device_name": format!("Terminal ({host})"), "public_key": public}))?;
        let device = paired.get("device_id").and_then(Value::as_str).context("no device id")?.to_string();
        let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(|e| anyhow!("key: {e}"))?;
        std::fs::create_dir_all(home)?;
        write_private(&key_path, pem.as_bytes())?;
        write_private(&device_path, device.as_bytes())?;
        Ok(Signer { device, key })
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    std::io::Write::write_all(&mut f, bytes)?;
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
        Client { base: format!("http://{host}:{port}"), agent, signer: None, service: None, timeout }
    }

    #[cfg(test)]
    pub(crate) fn at(base: String) -> Self {
        let mut client = Self::new(Duration::from_secs(5));
        client.base = base;
        client
    }

    /// A client whose requests are signed as the user: for the TUI and the commands the user
    /// types. Falls back to unsigned (and says so on stderr) when pairing is impossible.
    pub fn for_user(timeout: Duration) -> Self {
        let mut client = Self::new(timeout);
        match Signer::load_or_pair(&client) {
            Ok(signer) => client.signer = Some(signer),
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

    fn headers<B>(&self, mut req: ureq::RequestBuilder<B>, method: &str, path: &str, body: &[u8]) -> ureq::RequestBuilder<B> {
        if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") { req = req.header("x-alondra-orchestrator-execution", run); }
        if let Ok(sid) = std::env::var("ALONDRA_SESSION_ID").or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID")) { req = req.header("x-alondra-session", sid); }
        match &self.signer {
            Some(s) => s.sign(method, path, body).into_iter().fold(req, |r, (k, v)| r.header(k, v)),
            None => req,
        }
    }

    /// The raw answer, a refusal included: domain commands read their typed error from it.
    fn exchange(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        if let Some(service) = &self.service {
            return service.request(method, path, body, self.timeout);
        }
        let url = format!("{}{path}", self.base);
        let not_responding = tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)");
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let mut resp = match method {
            "POST" => self.headers(self.agent.post(url).header("content-type", "application/json"), method, path, &bytes).send(&bytes[..]),
            "PUT" => self.headers(self.agent.put(url).header("content-type", "application/json"), method, path, &bytes).send(&bytes[..]),
            "DELETE" => self.headers(self.agent.delete(url), method, path, b"").call(),
            _ => self.headers(self.agent.get(url), method, path, b"").call(),
        }
        .context(not_responding)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
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

    /// Fetch one server-rendered TUI view. The key is encoded by the URL form encoder
    /// because card identifiers and view parameters may contain reserved characters.
    pub fn views(&self, key: &str, known: Option<&str>) -> Result<ViewDocument> {
        let mut query = vec![("protocol", VIEWS_PROTOCOL.to_string()), ("client", "tui".to_owned()), ("lang", lang_name(crate::i18n::lang()).to_owned())];
        if let Some(version) = known { query.push(("known", version.to_owned())); }
        let query = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(query).finish();
        let path = format!("/views/{}?{query}", crate::mcp::urlencode(key));
        self.get(&path)
    }

    /// Poll changes for the open TUI views. Starting a newer request for the same device
    /// supersedes this one at the daemon.
    pub fn feed(&self, cursor: Option<&str>, views: &[(&str, &str)], wait: u64) -> Result<ViewsFeed> {
        let mut query = vec![
            ("protocol", VIEWS_PROTOCOL.to_string()),
            ("client", "tui".to_owned()),
            ("lang", lang_name(crate::i18n::lang()).to_owned()),
            ("wait", wait.min(50).to_string()),
        ];
        if let Some(cursor) = cursor { query.push(("cursor", cursor.to_owned())); }
        if !views.is_empty() {
            let open = views.iter().map(|(key, version)| format!("{key}@{version}")).collect::<Vec<_>>().join(",");
            query.push(("views", open));
        }
        let query = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(query).finish();
        self.get(&format!("/changes?{query}"))
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
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .body(bytes)?;
        let response = self.agent.run(request)?;
        answer(response)
    }

    /// The whole interaction, reassembled from chunks when its body did not fit one answer.
    pub fn interaction(&self, id: &InteractionId) -> Result<Interaction> {
        let id = crate::mcp::urlencode(&id.0);
        let detail: Item<Interaction> = self.get(&format!("/interactions/{id}"))?;
        let mut interaction = detail.item;
        if detail.truncated {
            let mut body = String::new();
            loop {
                let chunk: TextChunk = self.get(&format!("/interactions/{id}/body?offset={}", body.len()))?;
                body.push_str(&chunk.data);
                if !chunk.has_more {
                    break;
                }
            }
            interaction.body = body;
        }
        Ok(interaction)
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
    pub fn content(&self, route: &str) -> Result<Vec<u8>> {
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
        let url = format!("{}{path}", self.base);
        let not_responding = tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)");
        let mut resp = match method {
            "POST" => self.headers(self.agent.post(url).header("content-type", "application/octet-stream"), method, path, body).send(body),
            _ => self.headers(self.agent.get(url), method, path, b"").call(),
        }
        .context(not_responding)?;
        let status = resp.status().as_u16();
        let bytes = resp.body_mut().with_config().limit(MAX_ATTACHMENT_BYTES + 1024 * 1024).read_to_vec()?;
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
    })
}

/// Reads `route` from offset 0 to the end, refusing a file that changes between chunks.
fn content_via(route: &str, mut get: impl FnMut(&str) -> Result<Value>) -> Result<Vec<u8>> {
    use base64::Engine as _;
    let mut bytes = Vec::new();
    let mut version: Option<String> = None;
    let sep = if route.contains('?') { '&' } else { '?' };
    loop {
        let path = match &version {
            Some(v) => format!("{route}{sep}offset={}&version={}", bytes.len(), crate::mcp::urlencode(v)),
            None => format!("{route}{sep}offset=0"),
        };
        let chunk = get(&path)?;
        let v = chunk["version"].as_str().context("content version missing")?.to_string();
        anyhow::ensure!(version.as_ref().is_none_or(|old| *old == v) && chunk["offset"].as_u64() == Some(bytes.len() as u64), "the file changed while it was read");
        let total = chunk["total"].as_u64().context("content size missing")?;
        anyhow::ensure!(total <= MAX_ATTACHMENT_BYTES, "the file is larger than 20 MB");
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
    if let Ok(error) = serde_json::from_value::<DomainError>(body.clone()) {
        return anyhow::Error::new(Refused(error));
    }
    let msg = body.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| body.to_string());
    anyhow!(trf!("alondrad answered {status}: {msg}", "alondrad respondió {status}: {msg}"))
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
        "task_closing" => tr!("This task's pull request is already merged and the task is closing: ask for more work as a new request", "El PR de esta tarea ya está fusionado y la tarea se está cerrando: pide más trabajo como una petición nueva").into(),
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
            let headers = signer.sign("GET", "/views/home", b"");
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

    #[test]
    fn a_truncated_interaction_is_reassembled_from_its_chunks() {
        let interaction = serde_json::json!({"attachments": [], "id": "i-1", "scope": {"kind": "orchestrator", "request_id": null},
            "author": {"kind": "system"}, "recipient": {"kind": "user"}, "body": "cut", "blocking": false, "subject": null,
            "payload": {"kind": "information", "read_by": []}, "revision": 3, "order": 3,
            "card": {"kind": "know", "need": "Read the update"}});
        let chunk = |offset: usize, data: &str, more: bool| serde_json::json!({"protocol_version": 2, "data_epoch": "e", "revision": 3,
            "id": "i-1", "offset": offset, "total_bytes": 10, "data": data, "has_more": more});
        let (client, _, server) = peer(vec![
            ("/interactions/i-1", serde_json::json!({"protocol_version": 2, "data_epoch": "e", "revision": 3, "item": interaction, "truncated": true})),
            ("/interactions/i-1/body?offset=0", chunk(0, "first ", true)),
            ("/interactions/i-1/body?offset=6", chunk(6, "half", false)),
        ]);
        let full = client.interaction(&"i-1".into()).unwrap();
        assert_eq!(full.body, "first half");
        server.join().unwrap();
    }
}
