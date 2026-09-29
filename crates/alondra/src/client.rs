use std::sync::OnceLock;
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::domain_v2::reads::{Changes, Item, Page, Summary, TextChunk};
use alondra_core::domain_v2::{AttachmentRef, Command, CommandEnvelope, CommandOutcome, DataEpoch, DomainError, DomainProjection, Interaction, InteractionId, PROTOCOL_VERSION, Revision};
use std::collections::BTreeMap;
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

struct Signer {
    device: String,
    key: p256::ecdsa::SigningKey,
}

const KEY_FILE: &str = "tui-key.pem";
const DEVICE_FILE: &str = "tui-device";

impl Signer {
    fn sign(&self, method: &str, path: &str, body: &[u8]) -> [(&'static str, String); 3] {
        use base64::Engine as _;
        use p256::ecdsa::signature::Signer as _;
        use sha2::Digest as _;
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()).to_string();
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

    /// Domain producers use their daemon-issued bearer, never a locally paired User key.
    pub fn domain_get(&self, path: &str, bearer: &str) -> Result<Value> {
        let url = format!("http://127.0.0.1:{}{path}", config().port);
        let resp = self.agent.get(url).header("authorization", format!("Bearer {bearer}")).call()?;
        answer(resp)
    }

    pub fn domain_post(&self, path: &str, bearer: &str, body: &Value) -> Result<Value> {
        self.domain_call("POST", path, bearer, Some(body))
    }

    pub fn domain_call(&self, method: &str, path: &str, bearer: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("http://127.0.0.1:{}{path}", config().port);
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

    /// The domain assembled from its sections, without interactions: those are read one
    /// by one as the user opens them (`interaction`).
    pub fn domain(&self) -> Result<DomainProjection> {
        self.sync(&mut Sections::default())
    }

    pub fn summary(&self) -> Result<Summary> {
        self.get("/summary")
    }

    /// Every row of a section, page after page.
    pub fn walk<T: DeserializeOwned>(&self, base: &str) -> Result<(Vec<T>, Revision)> {
        let mut rows = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let path = match &cursor {
                Some(c) => format!("{base}{}cursor={c}", if base.contains('?') { "&" } else { "?" }),
                None => base.to_owned(),
            };
            let page: Page<T> = self.get(&path)?;
            rows.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok((rows, page.revision)),
            }
        }
    }

    /// Refreshes only the sections whose token moved since `cache` last saw them and
    /// assembles the projection at one revision.
    pub fn sync(&self, cache: &mut Sections) -> Result<DomainProjection> {
        let mut summary = self.summary()?;
        for attempt in 0.. {
            let mut moved = false;
            for (section, base) in SECTION_ROUTES {
                let token = &summary.sections[*section].revision;
                if cache.rows.get(*section).is_some_and(|(seen, _)| seen == token) {
                    continue;
                }
                let (rows, revision) = self.walk::<Value>(base)?;
                if revision != summary.revision {
                    moved = true;
                    break;
                }
                cache.rows.insert((*section).to_owned(), (token.clone(), rows));
            }
            if !moved {
                break;
            }
            anyhow::ensure!(attempt < 8, "the domain kept changing while it was being read");
            summary = self.summary()?;
        }
        cache.assemble(&summary)
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

    /// Waits up to `wait` seconds for the domain to move past `revision`.
    pub fn domain_changes(&self, epoch: &DataEpoch, revision: Revision, wait: u64) -> Result<Changes> {
        self.get(&format!("/changes?data_epoch={epoch}&after={revision}&wait={wait}"))
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

    /// The daemon's runtime readings; `None` from an alondrad that does not serve them yet.
    pub fn runtime(&self) -> Result<Option<crate::runtime::Runtime>> {
        match self.get::<Value>(crate::runtime::RUNTIME) {
            Ok(value) => Ok(Some(serde_json::from_value(value)?)),
            Err(e) if missing(&e) => Ok(None),
            Err(e) => Err(e),
        }
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

const SECTION_ROUTES: &[(&str, &str)] = &[
    ("inbox", "/inbox"),
    ("tasks", "/tasks"),
    ("requests", "/requests"),
    ("agents", "/agents"),
    ("pull_requests", "/pull-requests"),
    ("documents", "/documents"),
    ("merge_authorizations", "/merge-authorizations"),
    ("stack_readiness", "/stack-readiness"),
    ("superseded_acceptances", "/superseded-acceptances"),
];

/// Section rows by the token they were read at, so an unchanged section is never read twice.
#[derive(Default)]
pub struct Sections {
    rows: BTreeMap<String, (String, Vec<Value>)>,
}

impl Sections {
    fn assemble(&self, summary: &Summary) -> Result<DomainProjection> {
        let rows = |section: &str| -> Result<Vec<Value>> {
            Ok(self.rows.get(section).with_context(|| format!("section {section} was not read"))?.1.clone())
        };
        let documents = rows("documents")?;
        let mut projection = DomainProjection {
            protocol_version: summary.protocol_version,
            data_epoch: summary.data_epoch.clone(),
            revision: summary.revision,
            inbox: serde_json::from_value(Value::Array(rows("inbox")?))?,
            counts: summary.counts.clone(),
            tasks: serde_json::from_value(Value::Array(rows("tasks")?))?,
            agents: serde_json::from_value(Value::Array(rows("agents")?))?,
            requests: serde_json::from_value(Value::Array(rows("requests")?))?,
            interactions: vec![],
            pull_requests: serde_json::from_value(Value::Array(rows("pull_requests")?))?,
            documents: vec![],
            document_revisions: vec![],
            document_reviews: vec![],
            merge_authorizations: serde_json::from_value(Value::Array(rows("merge_authorizations")?))?,
            stack_readiness: serde_json::from_value(Value::Array(rows("stack_readiness")?))?,
            superseded_acceptances: serde_json::from_value(Value::Array(rows("superseded_acceptances")?))?,
        };
        for bundle in documents {
            let bundle: alondra_core::domain_v2::reads::DocumentBundle = serde_json::from_value(bundle)?;
            projection.documents.push(bundle.document);
            projection.document_revisions.extend(bundle.revisions);
            projection.document_reviews.extend(bundle.reviews);
        }
        Ok(projection)
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
        return anyhow!(refusal(&error));
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

pub fn envelope(epoch: &DataEpoch, expected_revision: Revision, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: epoch.clone(), command_id: new_id("tui").into(), expected_revision, command }
}

/// What a refusal means to the user; the code decides, the daemon's English message is the fallback.
pub fn refusal(error: &DomainError) -> String {
    match error.code.as_str() {
        "stale_revision" | "stale_epoch" => tr!("It changed since you saw it: check it and try again", "Ha cambiado desde que lo viste: revísalo y vuelve a intentarlo").into(),
        "not_found" => tr!("Alondra no longer has it", "Alondra ya no lo tiene").into(),
        "task_not_active" => tr!("The task is no longer active", "La tarea ya no está activa").into(),
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
}

#[cfg(test)]
mod section_tests {
    use super::*;
    use alondra_core::domain_v2::reads::{DocumentBundle, SectionInfo};
    use alondra_core::domain_v2::*;
    use std::collections::BTreeSet;

    fn seeded() -> DomainState {
        let mut state = DomainState::new("epoch-sections");
        let mut seq = 0;
        let mut apply = |state: &mut DomainState, actor: Actor, command: Command| {
            seq += 1;
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("c-{seq}").into(), expected_revision: state.revision, command };
            state.execute(&actor, &envelope).unwrap();
        };
        let user = Actor::User { user_id: "user-a".into() };
        let agent = Actor::Agent { agent_id: "agent-a".into() };
        apply(&mut state, user.clone(), Command::CreateRequest { request_id: "request-1".into(), body: "Synthetic request".into(), auto_merge: false });
        apply(&mut state, Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into() });
        apply(&mut state, user, Command::CreateTask { task_id: "task-1".into(), request_id: "request-1".into(), title: "Synthetic task".into(), agent_id: Some("agent-a".into()), review_prs: true, dependencies: BTreeSet::new(), stack: None });
        apply(&mut state, agent.clone(), Command::StartTask { task_id: "task-1".into() });
        apply(&mut state, agent.clone(), Command::CreateDocument { task_id: "task-1".into(), document_id: "doc-1".into(), title: "Design".into(), revision_id: "rev-1".into(), content_digest: "0".repeat(64), content_reference: "files/design.md".into(), role: "result".into() });
        apply(&mut state, agent.clone(), Command::RegisterPr { task_id: "task-1".into(), pr_id: "pr-1".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 7, head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
        apply(&mut state, agent, Command::PresentResult { attachments: vec![], task_id: "task-1".into(), interaction_id: "final-1".into(), body: "Result ready".into(), targets: vec![DeliveryTarget::PullRequest { pr_id: "pr-1".into(), head: "head1".into() }], is_final: true });
        state
    }

    #[test]
    fn sections_assemble_into_the_projection_the_daemon_serves() {
        let full = seeded().projection();
        let rows = |value: Value| value.as_array().unwrap().clone();
        let bundles: Vec<Value> = full.documents.iter().map(|d| serde_json::to_value(DocumentBundle {
            document: d.clone(),
            revisions: full.document_revisions.iter().filter(|r| r.document_id == d.id).cloned().collect(),
            reviews: full.document_reviews.iter().filter(|r| r.document_id == d.id).cloned().collect(),
        }).unwrap()).collect();
        let mut cache = Sections::default();
        for (section, value) in [
            ("inbox", serde_json::to_value(&full.inbox).unwrap()),
            ("tasks", serde_json::to_value(&full.tasks).unwrap()),
            ("requests", serde_json::to_value(&full.requests).unwrap()),
            ("agents", serde_json::to_value(&full.agents).unwrap()),
            ("pull_requests", serde_json::to_value(&full.pull_requests).unwrap()),
            ("documents", Value::Array(bundles)),
            ("merge_authorizations", serde_json::to_value(&full.merge_authorizations).unwrap()),
            ("stack_readiness", serde_json::to_value(&full.stack_readiness).unwrap()),
            ("superseded_acceptances", serde_json::to_value(&full.superseded_acceptances).unwrap()),
        ] {
            cache.rows.insert(section.into(), ("token".into(), rows(value)));
        }
        let summary = Summary { protocol_version: PROTOCOL_VERSION, data_epoch: full.data_epoch.clone(), revision: full.revision, counts: full.counts.clone(), open_tasks: 1, connected_agents: 0, sections: BTreeMap::from([("inbox".to_owned(), SectionInfo { revision: "token".into(), count: 1 })]), expansions: BTreeMap::new(), pending: vec![] };
        let assembled = cache.assemble(&summary).unwrap();
        let mut expected = full.clone();
        expected.interactions.clear();
        assert!(!full.documents.is_empty() && !full.document_revisions.is_empty() && !full.pull_requests.is_empty());
        assert_eq!(assembled, expected);
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

    fn page(section: &str, token: &str, revision: u64, items: Vec<Value>, next: Option<&str>) -> Value {
        let mut page = serde_json::json!({"protocol_version": 2, "data_epoch": "e", "revision": revision, "section": section,
            "section_revision": token, "total": items.len(), "items": items, "truncated": []});
        if let Some(next) = next {
            page["next_cursor"] = Value::String(next.into());
        }
        page
    }

    fn summary(revision: u64, inbox_token: &str) -> Value {
        let sections: serde_json::Map<String, Value> = SECTION_ROUTES.iter().map(|(name, _)| {
            let token = if *name == "inbox" { inbox_token } else { "same" };
            ((*name).to_owned(), serde_json::json!({"revision": token, "count": 0}))
        }).collect();
        serde_json::json!({"protocol_version": 2, "data_epoch": "e", "revision": revision,
            "counts": {"roots": 0, "pending": 0, "unread_information": 0}, "open_tasks": 0, "connected_agents": 0,
            "sections": sections})
    }

    fn task(id: &str) -> Value {
        serde_json::json!({"id": id, "request_id": "r-1", "agent_id": null, "title": id, "state": "running", "revision": 1,
            "started": true, "review_prs": false, "final_presentation": null, "pr_id": null, "dependencies": [],
            "stack": null, "documents": {}, "archived": false})
    }

    #[test]
    fn sync_walks_pages_and_rereads_only_the_sections_whose_token_moved() {
        let mut script = vec![
            ("/summary", summary(5, "a")),
            ("/inbox", page("inbox", "a", 5, vec![], None)),
            ("/tasks", page("tasks", "same", 5, vec![task("t-1")], Some("eyJpIjoidC0xIn0"))),
            ("/tasks?cursor=eyJpIjoidC0xIn0", page("tasks", "same", 5, vec![task("t-2")], None)),
        ];
        for (section, base) in &SECTION_ROUTES[2..] {
            script.push((base, page(section, "same", 5, vec![], None)));
        }
        script.push(("/summary", summary(6, "a2")));
        script.push(("/inbox", page("inbox", "a2", 6, vec![], None)));
        let (client, seen, server) = peer(script);
        let mut cache = Sections::default();
        let first = client.sync(&mut cache).unwrap();
        assert_eq!(first.revision, 5);
        assert_eq!(first.tasks.iter().map(|t| t.id.0.as_str()).collect::<Vec<_>>(), ["t-1", "t-2"]);
        let second = client.sync(&mut cache).unwrap();
        assert_eq!(second.revision, 6);
        assert_eq!(second.tasks.len(), 2, "an unchanged section is served from the cache");
        server.join().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.iter().filter(|p| p.starts_with("/tasks")).count(), 2);
        assert!(!seen.iter().any(|p| p == "/domain"), "the whole projection is never read");
    }

    #[test]
    fn a_truncated_interaction_is_reassembled_from_its_chunks() {
        let interaction = serde_json::json!({"attachments": [], "id": "i-1", "scope": {"kind": "orchestrator", "request_id": null},
            "author": {"kind": "system"}, "recipient": {"kind": "user"}, "body": "cut", "blocking": false, "subject": null,
            "payload": {"kind": "information", "read_by": []}, "revision": 3, "order": 3});
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
