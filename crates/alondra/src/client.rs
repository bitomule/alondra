use std::sync::OnceLock;
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::{Machine, Node, Question, Report};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The effective configuration; a broken `config.toml` falls back to defaults here and is
/// reported by `alondra doctor`.
pub fn config() -> &'static Config {
    CONFIG.get_or_init(|| Config::load().unwrap_or_else(|_| Config::defaults()))
}

#[derive(Deserialize, Clone)]
pub struct FleetNode {
    #[serde(default)]
    pub orchestrator_runtime: Option<OrchestratorRuntime>,
    #[serde(flatten)]
    pub node: Node,
    #[serde(default)]
    pub project: Option<String>,
}

/// Public status only; Claude conversation and execution identities stay internal.
#[derive(Deserialize, Clone, Default)]
pub struct OrchestratorRuntime {
    pub phase: String,
    #[serde(default)]
    pub can_retry: bool,
    #[serde(default)]
    pub can_cancel: bool,
    #[serde(default)]
    pub cancel_requested: bool,
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub queue_depth: usize,
    pub last_error: Option<String>,
    pub retry_at: Option<i64>,
}

impl FleetNode {
    pub fn status_label(&self) -> &'static str {
        match self.orchestrator_runtime.as_ref() {
            Some(runtime) if runtime.cancel_requested => tr!("stopping", "deteniendo"),
            Some(runtime) => match runtime.phase.as_str() {
                "idle" => tr!("available", "disponible"),
                "starting" => tr!("starting", "iniciando"),
                "running" => tr!("handling a request", "atendiendo una petición"),
                "compacting" => tr!("compacting context", "compactando contexto"),
                "failed" if runtime.retry_at.is_some() => tr!("retrying", "reintentando"),
                "failed" => tr!("needs attention", "requiere atención"),
                _ => tr!("unknown", "desconocido"),
            },
            None => crate::i18n::fleet_state(&self.node),
        }
    }
}

/// One thread of a node, the main one (no `agent_id`) or a subagent, and what it does now.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Activity {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    pub since: i64,
}

impl Activity {
    pub fn is_main(&self) -> bool {
        self.agent_id.is_none()
    }

    /// `main` for the node's own thread, the subagent's type otherwise.
    pub fn thread(&self) -> String {
        match (&self.agent_id, &self.agent_type) {
            (None, _) => tr!("main", "principal").into(),
            (Some(_), Some(t)) => t.clone(),
            (Some(_), None) => tr!("subagent", "subagente").into(),
        }
    }
}

/// A request with the state its tasks give it and their ids.
#[derive(Deserialize, Clone)]
pub struct ClientRequest {
    #[serde(flatten)]
    pub request: alondra_core::tasks::Request,
    pub state: alondra_core::tasks::RequestState,
    #[serde(default)]
    pub tasks: Vec<String>,
    /// The title the orchestrator gave it.
    #[serde(default)]
    pub title: Option<String>,
}

impl std::ops::Deref for ClientRequest {
    type Target = alondra_core::tasks::Request;
    fn deref(&self) -> &alondra_core::tasks::Request {
        &self.request
    }
}

#[derive(Deserialize, Clone)]
pub struct ClientTask {
    #[serde(flatten)]
    pub task: alondra_core::tasks::Task,
    /// Alondra merges its PR by itself once the checks pass.
    #[serde(default)]
    pub auto_merge_pending: bool,
    /// When anything last happened to it, its history included.
    #[serde(default)]
    pub last_change: Option<i64>,
}

impl std::ops::Deref for ClientTask {
    type Target = alondra_core::tasks::Task;
    fn deref(&self) -> &alondra_core::tasks::Task {
        &self.task
    }
}

impl ClientTask {
    /// This task's title next to its own node in the Fleet list or detail: itself, unless the
    /// title is only the node's own name repeated — the fallback a task gets until something
    /// names it for real (`TaskTitled`, t-47118). Then it reads as the node's main task, not the
    /// name shown twice.
    pub fn fleet_title(&self, node: &FleetNode) -> String {
        if self.title == node.display_name() { tr!("main task", "tarea principal").to_string() } else { self.title.clone() }
    }
}

/// Keeps every entry this build understands, so a state it does not know yet costs that entry,
/// not the whole overview.
fn lenient<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let values = Vec::<Value>::deserialize(d)?;
    Ok(values.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

impl std::ops::Deref for FleetNode {
    type Target = Node;
    fn deref(&self) -> &Node {
        &self.node
    }
}

#[derive(Deserialize, Clone, Default)]
pub struct Overview {
    #[serde(default)]
    pub delivery_attempts: Vec<crate::delivery::Attempt>,
    #[serde(default)]
    pub orchestrator: Option<OrchestratorRuntime>,
    pub tick: u64,
    pub nodes: Vec<FleetNode>,
    pub open_questions: Vec<OpenQuestion>,
    #[serde(default)]
    pub news: Vec<NewsReport>,
    pub machine: Machine,
    #[serde(default)]
    pub usage: Option<alondra_core::usage::Usage>,
    #[serde(default)]
    pub prs: Vec<PrCard>,
    /// The newest reports, read or not; a task's documents are looked up here.
    #[serde(default)]
    pub reports: Vec<Report>,
    #[serde(default, deserialize_with = "lenient")]
    pub requests: Vec<ClientRequest>,
    #[serde(default, deserialize_with = "lenient")]
    pub tasks: Vec<ClientTask>,
}

/// An open question, with the PR card it belongs to when that card is waiting on the user.
#[derive(Deserialize, Clone)]
pub struct OpenQuestion {
    #[serde(flatten)]
    pub question: Question,
    #[serde(default)]
    pub pr_id: Option<u64>,
    /// The task this question blocks.
    #[serde(default)]
    pub task: Option<String>,
    /// The report this question is about; it is shown inside the question, not as its own card.
    #[serde(default)]
    pub report: Option<Report>,
    /// Every report the question takes in, oldest first: its task's unread reports outside its PR
    /// card and its review cards.
    #[serde(default)]
    pub reports: Vec<Report>,
}

impl OpenQuestion {
    /// The reports shown inside the question; an older alondrad sends only `report`.
    pub fn about(&self) -> Vec<&Report> {
        if self.reports.is_empty() { self.report.iter().collect() } else { self.reports.iter().collect() }
    }
}

impl From<Question> for OpenQuestion {
    fn from(question: Question) -> Self {
        OpenQuestion { question, pr_id: None, task: None, report: None, reports: vec![] }
    }
}

impl std::ops::Deref for OpenQuestion {
    type Target = Question;
    fn deref(&self) -> &Question {
        &self.question
    }
}

/// An unread report, with the PR card it is shown inside when it links a PR that has one.
#[derive(Deserialize, Clone)]
pub struct NewsReport {
    #[serde(flatten)]
    pub report: Report,
    #[serde(default)]
    pub pr_id: Option<u64>,
    /// The task this report belongs to.
    #[serde(default)]
    pub task: Option<String>,
    /// The other unread reports of its task that no question carries, oldest first; this card
    /// carries them.
    #[serde(default)]
    pub earlier: Vec<Report>,
}

impl From<Report> for NewsReport {
    fn from(report: Report) -> Self {
        NewsReport { report, pr_id: None, task: None, earlier: vec![] }
    }
}

impl std::ops::Deref for NewsReport {
    type Target = Report;
    fn deref(&self) -> &Report {
        &self.report
    }
}

#[derive(Deserialize, Clone)]
pub struct PrCard {
    #[serde(flatten)]
    pub pr: alondra_core::pr::PullRequest,
    #[serde(default)]
    pub preferred_method: Option<alondra_core::pr::MergeMethod>,
    /// The number of the open PR this one is stacked on, which has to be merged first.
    #[serde(default)]
    pub below_number: Option<u64>,
    #[serde(default)]
    pub below_set_aside: bool,
    #[serde(default)]
    pub stack_bottom: Option<u64>,
    #[serde(default)]
    pub stack_opened_at: Option<i64>,
    #[serde(default)]
    pub stack_position: usize,
    /// The task this PR belongs to.
    #[serde(default)]
    pub task: Option<String>,
}

impl PrCard {
    /// Why it cannot be merged yet: the PR under it is still open on GitHub.
    pub fn merge_blocked(&self) -> Option<String> {
        let below = self.below_number?;
        Some(if self.below_set_aside {
            trf!("PR #{below} is still open on GitHub: merge or close it first", "El PR #{below} sigue abierto en GitHub: fusiónalo o ciérralo antes")
        } else {
            trf!("Merge PR #{below} first", "Fusiona antes el PR #{below}")
        })
    }

    /// Sorts trains together, bottom card first, trains by when their bottom card was opened.
    pub fn train_place(&self) -> (i64, u64, usize, u64) {
        (self.stack_opened_at.unwrap_or(self.pr.opened_at), self.stack_bottom.unwrap_or(self.pr.id), self.stack_position, self.pr.id)
    }

    /// The methods offered, the repo's preferred one first; all three when the repo was not read.
    pub fn methods(&self) -> Vec<alondra_core::pr::MergeMethod> {
        use alondra_core::pr::MergeMethod;
        let allowed = if self.pr.merge_methods.is_empty() { vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] } else { self.pr.merge_methods.clone() };
        let preferred = self.preferred_method.filter(|m| allowed.contains(m)).unwrap_or(allowed[0]);
        std::iter::once(preferred).chain(allowed.into_iter().filter(|m| *m != preferred)).collect()
    }

    /// The numbered actions of the card: merging in each method, or, while the branch conflicts
    /// with its base, only asking its node to resolve that.
    pub fn actions(&self) -> Vec<PrAction> {
        if self.pr.conflicts() {
            return vec![PrAction::ResolveConflicts];
        }
        self.methods().into_iter().map(PrAction::Merge).collect()
    }
}

/// alondrad answered 404 without saying why: it does not have that route yet.
#[derive(Debug)]
pub struct NotFound;

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(tr!("alondrad does not have this yet", "alondrad todavía no tiene esto"))
    }
}

impl std::error::Error for NotFound {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrAction {
    Merge(alondra_core::pr::MergeMethod),
    ResolveConflicts,
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
        let code = unsigned.post("/v1/pairing", &serde_json::json!({}))?.get("code").and_then(Value::as_str).map(str::to_string).context("no pairing code")?;
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes());
        let host = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "Mac".into());
        let paired = unsigned.post("/v1/pair", &serde_json::json!({"code": code, "device_name": format!("Terminal ({host})"), "public_key": public}))?;
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

    fn service_result(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        let (status, value) = self.service.as_ref().context("no service destination")?.request(method, path, body, self.timeout)?;
        if status >= 400 { return Err(Self::failure(status, serde_json::to_string(&value)?)); }
        Ok((status, value))
    }

    fn signed(&self, req: ureq::RequestBuilder<ureq::typestate::WithoutBody>, method: &str, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let mut req = req;
        if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") { req = req.header("x-alondra-orchestrator-execution", run); }
        if let Ok(sid) = std::env::var("ALONDRA_SESSION_ID").or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID")) { req = req.header("x-alondra-session", sid); }
        match &self.signer {
            Some(s) => s.sign(method, path, b"").into_iter().fold(req, |r, (k, v)| r.header(k, v)),
            None => req,
        }
    }

    fn check(mut resp: ureq::http::Response<ureq::Body>) -> Result<(u16, String)> {
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        if status >= 400 {
            return Err(Self::failure(status, body));
        }
        Ok((status, body))
    }

    /// Why alondrad said no, in the user's language where the reason is one the apps know.
    fn failure(status: u16, body: String) -> anyhow::Error {
        let parsed = serde_json::from_str::<Value>(&body).ok();
        if status == 404 && parsed.as_ref().and_then(|v| v.get("error")).is_none() {
            return anyhow::Error::new(NotFound);
        }
        if status == 404 {
            return anyhow!(tr!("Alondra no longer has it", "Alondra ya no lo tiene"));
        }
        if parsed.as_ref().and_then(|v| v.get("kind")).and_then(Value::as_str) == Some("task_refused") {
            return anyhow!(tr!("The task’s state does not allow that now", "El estado de la tarea no lo permite ahora"));
        }
        let reasons: Vec<String> = parsed
            .as_ref()
            .and_then(|v| v.get("reasons"))
            .and_then(Value::as_array)
            .map(|rs| {
                rs.iter()
                    .map(|r| crate::i18n::close_reason(r.get("kind").and_then(Value::as_str).unwrap_or_default(), r.get("detail").and_then(Value::as_str)))
                    .collect()
            })
            .unwrap_or_default();
        if !reasons.is_empty() {
            let text = match reasons.as_slice() {
                [one] => one.clone(),
                many => many.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n"),
            };
            return anyhow!(text);
        }
        let msg = parsed.as_ref().and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string)).unwrap_or(body);
        anyhow!(trf!("alondrad answered: {msg}", "alondrad respondió: {msg}"))
    }

    pub fn get_text(&self, path: &str) -> Result<(u16, String)> {
        if self.service.is_some() {
            let (status, value) = self.service_result("GET", path, None)?;
            let text = if path.starts_with("/v1/file?") { value["text"].as_str().context("file text missing")?.to_string() } else { serde_json::to_string(&value)? };
            return Ok((status, text));
        }
        let resp = self.signed(self.agent.get(format!("{}{path}", self.base)), "GET", path).call().context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        Self::check(resp)
    }

    /// The raw body, for files that may not be text (images, PDFs).
    pub fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        if let Some(service) = &self.service { return service.file(path, self.timeout); }
        let mut resp = self.signed(self.agent.get(format!("{}{path}", self.base)), "GET", path).call().context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        if resp.status().as_u16() >= 400 {
            return Self::check(resp).map(|(_, body)| body.into_bytes());
        }
        Ok(resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec()?)
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let (_, body) = self.get_text(path)?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        if self.service.is_some() { return self.service_result("POST", path, Some(body)).map(|(_, v)| v); }
        let bytes = serde_json::to_vec(body)?;
        let mut req = self.agent.post(format!("{}{path}", self.base)).header("content-type", "application/json");
        if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") { req = req.header("x-alondra-orchestrator-execution", run); }
        if let Ok(sid) = std::env::var("ALONDRA_SESSION_ID").or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID")) { req = req.header("x-alondra-session", sid); }
        if let Some(s) = &self.signer {
            for (k, v) in s.sign("POST", path, &bytes) {
                req = req.header(k, v);
            }
        }
        let resp = req.send(&bytes[..]).context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        let (_, text) = Self::check(resp)?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub fn put(&self, path: &str, body: &Value) -> Result<Value> {
        if self.service.is_some() { return self.service_result("PUT", path, Some(body)).map(|(_, v)| v); }
        let bytes = serde_json::to_vec(body)?;
        let mut req = self.agent.put(format!("{}{path}", self.base)).header("content-type", "application/json");
        if let Some(s) = &self.signer {
            for (k, v) in s.sign("PUT", path, &bytes) { req = req.header(k, v); }
        }
        let resp = req.send(&bytes[..]).context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        let (_, text) = Self::check(resp)?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Copies a file into Alondra's storage; the answer carries the upload's id.
    pub fn upload(&self, name: &str, bytes: &[u8]) -> Result<Value> {
        if let Some(service) = &self.service { return service.upload(name, bytes, self.timeout); }
        let path = format!("/v1/uploads?name={}", crate::mcp::urlencode(name));
        let mut req = self.agent.post(format!("{}{path}", self.base)).header("content-type", "application/octet-stream");
        if let Some(s) = &self.signer {
            for (k, v) in s.sign("POST", &path, bytes) {
                req = req.header(k, v);
            }
        }
        let resp = req.send(bytes).context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        let (_, text) = Self::check(resp)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        if self.service.is_some() { return self.service_result("DELETE", path, None).map(|_| ()); }
        let resp = self.signed(self.agent.delete(format!("{}{path}", self.base)), "DELETE", path).call().context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        Self::check(resp).map(|_| ())
    }

    pub fn overview(&self) -> Result<Overview> {
        if let Some(service) = &self.service {
            let (status, value) = crate::overview_snapshot::read_transport(service.as_ref(), self.timeout)?;
            if status >= 400 { return Err(Self::failure(status, serde_json::to_string(&value)?)); }
            return Ok(serde_json::from_value(value)?);
        }
        self.get("/v1/overview")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_for_this_thread};

    #[test]
    fn a_refused_task_action_and_something_gone_read_in_the_users_language() {
        set_for_this_thread(Lang::Es);
        let said = |status, body: &str| Client::failure(status, body.to_string()).to_string();
        assert_eq!(said(409, r#"{"error":"the task is already done","kind":"task_refused"}"#), "El estado de la tarea no lo permite ahora");
        assert_eq!(said(404, r#"{"error":"task t-9 does not exist"}"#), "Alondra ya no lo tiene");
        assert!(Client::failure(404, String::new()).downcast_ref::<NotFound>().is_some(), "a bare 404 is a route this alondrad lacks");
        assert_eq!(said(409, r#"{"error":"something new"}"#), "alondrad respondió: something new");
    }
}

#[cfg(test)]
mod orchestrator_contract_tests {
    use super::*;

    #[test]
    fn idle_runtime_decodes_without_a_claude_session_and_is_not_offline() {
        let mut value = serde_json::to_value(Node::new("alondra-orchestrator", 1)).unwrap();
        value["is_orchestrator"] = serde_json::json!(true);
        value["alive"] = serde_json::json!(false);
        value["orchestrator_runtime"] = serde_json::json!({"phase": "idle", "queue_depth": 2});
        let node: FleetNode = serde_json::from_value(value).unwrap();
        assert!(!node.alive);
        assert_eq!(node.orchestrator_runtime.as_ref().unwrap().queue_depth, 2);
        assert_eq!(node.status_label(), tr!("available", "disponible"));
    }
}
