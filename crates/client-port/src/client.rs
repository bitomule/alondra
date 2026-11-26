//! The daemon client every adapter uses: requests, views, the feed, actions, domain commands and
//! files, and how a failure reads. Which route a request takes is the installed transport's.
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use alondra_core::config::Config;
use alondra_domain_model::{AttachmentRef, Command, CommandEnvelope, CommandOutcome, DataEpoch, DomainError, PROTOCOL_VERSION, Revision};
use alondra_view_model::{View as ViewDocument, VIEWS_PROTOCOL};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::transport::{DirectMac, Local, Remote, Request, transport};

static CONFIG: OnceLock<Config> = OnceLock::new();

pub mod availability;

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

/// The route a client's requests take when it is not this Mac's daemon over HTTP.
#[derive(Clone)]
enum Away {
    /// The Mac chosen through the relay: files and uploads go in chunks.
    Relay(Arc<dyn Remote>),
    Direct(Arc<dyn DirectMac>),
}

#[derive(Clone)]
pub struct Client {
    local: Arc<dyn Local>,
    away: Option<Away>,
    timeout: Duration,
    protocol: u32,
    mcp_operation: Option<String>,
    capabilities: Arc<Mutex<alondra_view_model::ClientCapabilities>>,
    response_observer: Option<availability::ResponseObserver>,
}

/// The version the local daemon stamped on the first answer this process got from it.
static SERVICE_VERSION: OnceLock<String> = OnceLock::new();

pub fn service_version() -> Option<&'static str> {
    SERVICE_VERSION.get().map(String::as_str)
}

/// Keeps the version the local daemon stamped on an answer; the first one stands.
pub fn note_service_version(version: &str) {
    let _ = SERVICE_VERSION.set(version.to_owned());
}

/// Which Mac a TUI instance talks to: this one over loopback, or one paired directly.
#[derive(Clone)]
pub enum Target {
    Local,
    Direct(Arc<dyn DirectMac>),
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
    pub status: alondra_view_model::Status,
    #[serde(default)]
    pub stale: Vec<String>,
    #[serde(default)]
    pub reset: bool,
    #[serde(default)]
    pub superseded: bool,
    #[serde(default)]
    pub service: Option<alondra_view_model::Service>,
}

impl Client {
    /// A client of this Mac's daemon over `local`, as the transport builds one.
    pub fn over(local: Arc<dyn Local>, timeout: Duration) -> Self {
        Client { local, away: None, timeout, protocol: VIEWS_PROTOCOL, mcp_operation: None, capabilities: Default::default(), response_observer: None }
    }

    pub fn new(timeout: Duration) -> Self {
        Self::over(transport().local(timeout, false), timeout)
    }

    /// A client of the daemon at `base`, unsigned: for tests that run a fake one.
    pub fn at(base: String) -> Self {
        Self::over(transport().at(base), Duration::from_secs(5))
    }

    pub fn bounded(&self, timeout: Duration) -> Self {
        let mut client = Self::over(self.local.bounded(timeout), timeout);
        client.away = self.away.clone();
        client.protocol = self.protocol;
        client.capabilities = self.capabilities.clone();
        client
    }

    /// A client whose requests are signed as the user: for the TUI and the commands the user
    /// types. Falls back to unsigned (and says so on stderr) when pairing is impossible.
    pub fn for_user(timeout: Duration) -> Self {
        Self::over(transport().local(timeout, true), timeout)
    }

    /// Only interactive TUI requests consult its explicitly selected remote destination.
    pub fn for_tui(timeout: Duration) -> Self {
        if let Some(relay) = transport().relay() {
            let mut client = Self::new(timeout.max(Duration::from_secs(15)));
            client.away = Some(Away::Relay(relay));
            client
        } else { Self::for_user(timeout) }
    }

    /// A client for one TUI instance's Mac. A direct Mac gets the longer wait its network needs.
    pub fn for_target(target: &Target, timeout: Duration) -> Self {
        match target {
            Target::Local => Self::for_tui(timeout),
            Target::Direct(connection) => {
                let mut client = Self::new(timeout.max(Duration::from_secs(15)));
                client.away = Some(Away::Direct(connection.clone()));
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
        Self { local: self.local.clone(), away: self.away.clone(), timeout: self.timeout,
            protocol: self.protocol, mcp_operation: Some(identity.into()), capabilities: self.capabilities.clone(), response_observer: None }
    }

    pub fn operation_id(&self) -> Option<&str> { self.mcp_operation.as_deref() }

    fn through_relay(&self) -> bool {
        matches!(self.away, Some(Away::Relay(_)))
    }

    /// The raw answer, a refusal included: domain commands read their typed error from it.
    pub fn exchange(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        if let Some(observer) = &self.response_observer { observer.begin_request(); }
        match &self.away {
            Some(Away::Relay(relay)) => return relay.request(method, path, body, self.timeout, self.response_observer.as_ref()),
            Some(Away::Direct(direct)) => return direct.request(method, path, body, self.timeout, self.response_observer.as_ref()),
            None => {}
        }
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let (status, answer) = self.local(method, path, "application/json", &bytes)?;
        Ok((status, serde_json::from_slice(&answer).unwrap_or(Value::Null)))
    }

    /// A request to this Mac's alondrad.
    fn local(&self, method: &str, path: &str, content_type: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
        self.local.send(Request { method, path, content_type, body, operation: self.operation_id() }, self.response_observer.as_ref())
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
        let path = format!("/views/{}?{query}", crate::routes::urlencode(base));
        self.get(&path)
    }

    /// Poll changes for the open TUI views. Concurrent terminals can share one device identity.
    pub fn feed(&self, cursor: Option<&str>, views: &[(&str, &str)], wait: u64) -> Result<ViewsFeed> {
        let reply = self.feed_with_capabilities(cursor, views, wait)?;
        if self.observe_capabilities(reply.service.as_ref()) {
            let mut fresh = self.feed_with_capabilities(None, &[], 0)?;
            fresh.reset = true;
            return Ok(fresh);
        }
        Ok(reply)
    }

    fn observe_capabilities(&self, service: Option<&alondra_view_model::Service>) -> bool {
        let supported = service.map(|service| service.capabilities.iter().filter(|cap| matches!(cap.as_str(), "provider_accounts" | "model_selection" | "account_presentation" | "spec_fields")).cloned().collect::<Vec<_>>().join(",")).unwrap_or_default();
        let next = alondra_view_model::ClientCapabilities::parse(&supported).unwrap_or_default();
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

    /// Domain producers use their daemon-issued bearer, never a locally paired User key.
    pub fn domain_get(&self, path: &str, bearer: &str) -> Result<Value> {
        let (status, bytes) = self.local.domain_get(path, bearer)?;
        answer(status, &bytes)
    }

    pub fn domain_post(&self, path: &str, bearer: &str, body: &Value) -> Result<Value> {
        self.domain_call("POST", path, bearer, Some(body))
    }

    pub fn domain_call(&self, method: &str, path: &str, bearer: &str, body: Option<&Value>) -> Result<Value> {
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let (status, daemon, answered) = self.local.domain_call(Request { method, path, content_type: "application/json", body: &bytes, operation: self.operation_id() }, bearer)?;
        answer(status, &answered).map_err(|refusal| match daemon.filter(|daemon| refused_for_being_behind(status, daemon)) {
            Some(daemon) => anyhow::Error::new(DaemonBehind { daemon, refusal }),
            None => refusal,
        })
    }

    /// A refusal is the daemon's typed answer; anything else leaves the outcome unknown.
    pub fn command(&self, envelope: &CommandEnvelope) -> Result<CommandOutcome, CommandFailure> {
        let body = serde_json::to_value(envelope).map_err(|e| CommandFailure::Uncertain(e.to_string()))?;
        let kind = body.pointer("/command/kind").and_then(Value::as_str)
            .ok_or_else(|| CommandFailure::Uncertain("serialized command has no kind".into()))?;
        let (method, path) = crate::routes::resource_target(kind, &body["command"])
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
            stored = format!("{FILES}?path={}", crate::routes::urlencode(route));
            &stored
        } else {
            route
        };
        match route.strip_prefix(FILES) {
            Some(query) if self.through_relay() => content_via(&format!("{FILES}/chunk{query}"), |path| self.get::<Value>(path)),
            Some(_) => self.raw("GET", route, &[]),
            None => content_via(route, |path| self.get::<Value>(path)),
        }
    }

    /// Copies a file into the daemon's storage and returns the reference commands attach:
    /// in one request on this Mac, in chunks through the service.
    pub fn upload(&self, name: &str, bytes: &[u8]) -> Result<AttachmentRef> {
        if !self.through_relay() {
            let answer = self.raw("POST", &format!("{UPLOADS}?name={}", crate::routes::urlencode(name)), bytes)?;
            return Ok(serde_json::from_slice(&answer)?);
        }
        let finished = upload_in_chunks(name, bytes, self.timeout.max(Duration::from_secs(120)), |step, body, _| self.post(&format!("{SERVICE_UPLOADS}/{step}"), &body))?;
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

/// Uploads `bytes` in 64 KB chunks through `call` (`start`, each `chunk`, `finish`), each step
/// before `timeout` runs out; on a failure it says how far it got, never what it carried.
pub fn upload_in_chunks(name: &str, bytes: &[u8], timeout: Duration,
    mut call: impl FnMut(&str, Value, Instant) -> Result<Value>) -> Result<Value> {
    let started = Instant::now();
    let deadline = started + timeout;
    if bytes.is_empty() || bytes.len() > 20 * 1024 * 1024 {
        bail!("attachment must contain 1 byte to 20 MB");
    }
    let id = random::<16>()?;
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    let id = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    let hash: String = sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut phase = "start";
    let mut confirmed = 0usize;
    let mut accepted = false;
    let result = (|| {
        call("start", json!({"transfer_id":id,"name":name,"bytes":bytes.len(),"sha256":hash}), deadline)?;
        accepted = true;
        phase = "chunk";
        for (index, chunk) in bytes.chunks(64 * 1024).enumerate() {
            call("chunk", json!({"transfer_id":id,"offset":index * 64 * 1024,"data":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(chunk)}), deadline)?;
            confirmed += chunk.len();
        }
        phase = "finish";
        call("finish", json!({"transfer_id":id}), deadline)
    })();
    // Snapshot before best-effort cancellation: never expose the name, path,
    // transfer ID, credentials or payload in the progress diagnostic.
    let result = result.with_context(|| format!(
        "attachment upload failed: phase={phase}, bytes_confirmed={confirmed}/{}, elapsed_ms={}",
        bytes.len(), started.elapsed().as_millis(),
    ));
    if result.is_err() && accepted {
        let _ = call("cancel", json!({"transfer_id":id}), deadline);
    }
    result
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
    format!("{FILES}?path={}", crate::routes::urlencode(&a.content_reference))
}

pub fn document_content(revision: &str) -> String {
    format!("/document-revisions/{}/content", crate::routes::urlencode(revision))
}

/// Reads `route` from offset 0 to the end, refusing a file that changes between chunks.
fn content_via(route: &str, mut get: impl FnMut(&str) -> Result<Value>) -> Result<Vec<u8>> {
    use base64::Engine as _;
    let mut bytes = Vec::new();
    let mut version: Option<String> = None;
    let sep = if route.contains('?') { '&' } else { '?' };
    let limit = if route.starts_with("/interactions/") { alondra_domain_model::MAX_ATTACHMENT_BYTES } else { MAX_ATTACHMENT_BYTES };
    loop {
        let path = match &version {
            Some(v) => format!("{route}{sep}offset={}&version={}", bytes.len(), crate::routes::urlencode(v)),
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

fn answer(status: u16, body: &[u8]) -> Result<Value> {
    let value = serde_json::from_slice(body).unwrap_or(Value::Null);
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
pub fn failure(status: u16, body: &Value) -> anyhow::Error {
    if status == 404 && (body.is_null() || body.as_object().is_some_and(|o| o.is_empty())) {
        return anyhow::Error::new(Missing);
    }
    if body["code"] == "incompatible_protocol" {
        let service = serde_json::from_value(body["service"].clone()).ok();
        return anyhow::Error::new(Incompatible { service });
    }
    match alondra_view_model::action_outcome::Pending::of(body) {
        Some(alondra_view_model::action_outcome::Pending::InProgress) => return anyhow::Error::new(Running),
        Some(alondra_view_model::action_outcome::Pending::Unknown(reason)) => return anyhow::Error::new(OutcomeUnknown(reason)),
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
    pub service: Option<alondra_view_model::Service>,
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
pub fn pick_protocol(serves: alondra_view_model::ProtocolRange) -> Option<u32> {
    let top = VIEWS_PROTOCOL.min(serves.max);
    (top >= alondra_view_model::VIEWS_PROTOCOL_MIN.max(serves.min)).then_some(top)
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
pub struct OutcomeUnknown(pub alondra_view_model::action_outcome::UnknownReason);

impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let outcome = alondra_view_model::unknown_outcome(self.0, crate::i18n::lang());
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
    Unknown(alondra_view_model::action_outcome::UnknownReason),
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
        if let Some(found) = crate::transport::installed().and_then(|transport| transport.delivery(cause)) {
            return found;
        }
    }
    Delivery::Unanswered
}

/// Whether the transport failed somewhere among the error's causes, so no answer came back.
pub fn transport_failure(error: &anyhow::Error) -> bool {
    let Some(transport) = crate::transport::installed() else { return false };
    error.chain().any(|cause| transport.delivery(cause).is_some())
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

/// `N` bytes from the system's secure randomness.
pub fn random<const N: usize>() -> Result<[u8; N]> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0; N];
    ring::rand::SystemRandom::new().fill(&mut bytes).map_err(|_| anyhow!("OS randomness failed"))?;
    Ok(bytes)
}

/// A fresh identifier for a command or for an entity the command creates.
pub fn new_id(prefix: &str) -> String {
    let bytes = random::<16>().unwrap_or_default();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{hex}")
}

/// Views actions require a UUID v4 so the daemon can deduplicate retries.
fn new_views_command_id() -> String {
    let mut bytes = random::<16>().expect("secure command randomness");
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
        let contract = crate::fixture("requests.json")["attachment_content"]["response"].clone();
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
        assert_eq!(content_via(video, first(alondra_domain_model::MAX_ATTACHMENT_BYTES + 1)).unwrap_err().to_string(), "the file is larger than 64 MB");
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

    #[test]
    fn still_running_and_unknown_outcomes_are_read_from_their_kind_never_shown_as_their_code() {
        use alondra_view_model::action_outcome::{UnknownReason, in_progress_body, unknown_body};
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
