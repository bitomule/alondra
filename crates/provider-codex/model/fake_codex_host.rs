//! A model of the Codex host: the persistent host with its Codex session (`agent-host/
//! codex-session.ts`) and the repository's fake native app-server behind it (`fake_codex_native`),
//! played in the test's own process. It speaks the daemon's wire with the real codec and proofs
//! (`host_transport`, shared with the Claude model) and says what the Codex host says: every native
//! notification raw, the session's own `alondra/*` events, decisions and their settlements, and how
//! the app-server exited.
//!
//! The native requests the session sends go to the fake app-server, which writes them to the
//! transcript the test names, so a test reads the same native requests whichever hosts it runs on.
//! What the real session does asynchronously runs here in its order: what needs no native round
//! trip before the receipt of the command that caused it, the rest after it.
//!
//! Gaps it leaves: no orchestrator host, no polling of a managed child that is still running, no
//! code-cell control refusal, and the app-server never dies by itself.

use alondra_codex_protocol as protocol;
use alondra_engine_doubles::agent_host::{Serve, Served, Stop};
use fake_codex_native::{Native, Printed};
use host_transport::{Bootstrap, Doubled, HostState, Identity, Wire, lock};
use protocol::{Action, Command, Decision, DecisionKind};
use serde_json::{Map, Value, json};
use sha2::Digest as _;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod fake_codex_native;
#[path = "../../provider-claude/model/host_transport.rs"]
mod host_transport;

/// What the Codex host announces in its hello.
const CAPABILITIES: &[&str] = &[
    "streaming_input", "exact_decisions", "owner_attach", "commit_ack", "interrupt", "force_stop", "set_model", "set_permission_mode", "mcp_status", "mcp_sign_in",
    "native_account_policy", "causal_child_continuations", "plan_supersession",
];
const MAX_QUEUED: usize = 64;
const MAX_PROMPT: usize = 900_000;
const ROLE_ENVIRONMENT: &[&str] = &[
    "ALONDRA_HOME", "ALONDRA_DOMAIN_CAPABILITY", "ALONDRA_DOMAIN_PROTOCOL", "ALONDRA_SESSION_ID", "ALONDRA_ORCHESTRATOR", "ALONDRA_ORCHESTRATOR_EXECUTION", "ALONDRA_PORT",
    "ALONDRA_SANDBOX", "ALONDRA_BIN", "ALONDRA_PROVIDER",
];
const APPROVED_PLAN: &str = "Implement the plan the user just approved. Continue the Alondra task and report its result through Alondra.";

pub type Host = host_transport::Host<Session>;
type State = HostState<Session>;

/// The model itself, shared by whatever starts its hosts and the test that reads them.
#[derive(Clone)]
pub struct CodexModel(Arc<Shared>);

struct Shared {
    hosts: Mutex<Vec<Arc<Host>>>,
    activity: AtomicU64,
    starting: Arc<AtomicU64>,
    trouble: Mutex<Option<String>>,
    clock: alondra_core::clock::Clock,
    runtime: Option<tokio::runtime::Handle>,
    closed: AtomicBool,
}

impl Default for CodexModel {
    fn default() -> Self {
        Self(Arc::new(Shared {
            hosts: Mutex::default(),
            activity: AtomicU64::new(0),
            starting: Arc::new(AtomicU64::new(0)),
            trouble: Mutex::default(),
            clock: alondra_core::clock::Clock::current(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            closed: AtomicBool::new(false),
        }))
    }
}

/// Whether a host launched with `bootstrap` is a Codex host.
pub fn plays(bootstrap: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bootstrap).is_ok_and(|bootstrap| bootstrap["manifest"]["sdk_version"] == protocol::SDK_VERSION)
}

impl CodexModel {
    pub fn activity(&self) -> u64 {
        self.0.activity.load(Ordering::SeqCst)
    }

    /// What went wrong, or what a host was asked that the model does not play.
    pub fn trouble(&self) -> Option<String> {
        lock(&self.0.trouble).clone()
    }

    /// Every host started for `task`, in the order they started.
    pub fn hosts_of(&self, task: &str) -> Vec<Arc<Host>> {
        lock(&self.0.hosts).iter().filter(|host| host.task == task).cloned().collect()
    }

    /// Whether the daemon is attached to every host that runs and has acknowledged what each said.
    pub fn committed(&self) -> bool {
        self.0.starting.load(Ordering::SeqCst) == 0 && lock(&self.0.hosts).iter().all(|host| host.settled())
    }

    /// Whether the host whose process goes by `process_id` still runs.
    pub fn runs(&self, process_id: u32) -> bool {
        self.host(process_id).is_some_and(|host| host.alive())
    }

    /// The host whose process goes by `process_id` dies, with its app-server.
    pub fn kill_process(&self, process_id: u32) {
        if let Some(host) = self.host(process_id) {
            host.end(false);
        }
    }

    /// Ends every host and starts no other: once it returns, no host of the model writes anything.
    pub fn close(&self) {
        let hosts = {
            let hosts = lock(&self.0.hosts);
            self.0.closed.store(true, Ordering::SeqCst);
            hosts.clone()
        };
        for host in hosts {
            host.end(false);
        }
    }

    fn host(&self, process_id: u32) -> Option<Arc<Host>> {
        lock(&self.0.hosts).iter().find(|host| host.process_id == process_id).cloned()
    }

    fn tick(&self) {
        self.0.activity.fetch_add(1, Ordering::SeqCst);
    }

    fn troubled(&self, why: String) {
        lock(&self.0.trouble).get_or_insert(why);
    }

    fn spawn(&self, work: impl FnOnce() + Send + 'static) {
        let (clock, runtime) = (self.0.clock, self.0.runtime.clone());
        std::thread::spawn(move || {
            let _entered = alondra_core::clock::enter(clock, runtime.as_ref());
            work();
        });
    }

    fn start(&self, served: &Served) -> io::Result<Arc<Host>> {
        let (bootstrap, secret) = Bootstrap::read(served.bootstrap())?;
        let Some(task) = bootstrap.task() else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the model plays no orchestrator host"));
        };
        let identity = Identity {
            account: bootstrap.fence.account_id.clone(),
            capability: bootstrap.native.env.get("ALONDRA_DOMAIN_CAPABILITY").cloned().unwrap_or_default(),
            session: bootstrap.native.session_id.clone(),
            resumes: bootstrap.resumes(),
            process_id: served.process_id(),
            task,
        };
        let host = Arc::new(Host::new(identity, (bootstrap, secret), CAPABILITIES, Box::new(Doubled(served.clone())), self.0.starting.clone(), Session { process: Some(served.clone()), ..Session::default() }));
        {
            let mut hosts = lock(&self.0.hosts);
            if self.0.closed.load(Ordering::SeqCst) {
                drop(hosts);
                host.end(false);
                return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "the model starts no more hosts"));
            }
            hosts.push(host.clone());
        }
        self.tick();
        Ok(host)
    }

    /// The host was asked to stop with a signal: its session closes, after interrupting the turn it
    /// runs when the signal was SIGINT, and the host ends its whole group.
    fn stopped(&self, served: &Served, stop: Stop) {
        if let Some(host) = self.host(served.process_id()) {
            let mut state = lock(&host.state);
            let HostState { wire, play } = &mut *state;
            let mut out = Out { model: self, host: &host, wire };
            if stop == Stop::AfterInterrupt {
                play.interrupt(&mut out);
            }
            play.close(&mut out);
            play.drive(&mut out, Phase::All);
        }
        served.end(Stop::Kill.status());
    }
}

impl host_transport::Model for CodexModel {
    type Play = Session;

    fn tick(&self) {
        CodexModel::tick(self);
    }

    fn troubled(&self, why: String) {
        CodexModel::troubled(self, why);
    }

    fn command(&self, host: &Host, state: &mut State, command: Command) {
        let HostState { wire, play } = state;
        let mut out = Out { model: self, host, wire };
        play.command(&mut out, &command);
    }
}

/// The model plays every doubled Codex host: the first connection to one starts its host from the
/// bootstrap it was launched with, and each connection is served on a thread of its own.
impl Serve for CodexModel {
    fn connect(&self, served: &Served, stream: UnixStream) {
        let host = match self.host(served.process_id()) {
            Some(host) => host,
            None => match self.start(served) {
                Ok(host) => {
                    host_transport::ends_with(served, &host);
                    host
                }
                Err(error) => {
                    self.troubled(format!("a doubled Codex host's bootstrap: {error}"));
                    return served.hold(stream);
                }
            },
        };
        if !host.hold(&stream) {
            return;
        }
        let model = self.clone();
        self.spawn(move || host_transport::serve(&model, &host, stream));
    }

    fn stop(&self, served: &Served, stop: Stop) {
        match stop {
            Stop::Kill => served.end(stop.status()),
            Stop::Request | Stop::AfterInterrupt => self.stopped(served, stop),
        }
    }
}

/// Where what a host's session says goes: its wire, under the host's lock.
struct Out<'a> {
    model: &'a CodexModel,
    host: &'a Host,
    wire: &'a mut Wire,
}

impl<'a> Out<'a> {
    fn bootstrap(&self) -> &'a Bootstrap {
        &self.host.bootstrap
    }

    /// Journals `observation` and returns its cursor.
    fn observe(&mut self, observation: Value) -> u64 {
        match self.wire.event(&self.host.bootstrap, observation) {
            Ok(()) => self.model.tick(),
            Err(why) => self.model.troubled(why),
        }
        self.wire.cursor
    }

    fn receipt(&mut self, command: &Command, outcome: Value) {
        match self.wire.receipt(&self.host.bootstrap, command, outcome) {
            Ok(()) => self.model.tick(),
            Err(why) => self.model.troubled(why),
        }
    }

    fn troubled(&self, why: String) {
        self.model.troubled(format!("the Codex host of {}: {why}", self.host.task));
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Default,
    Plan,
    DontAsk,
}

/// How the session's opening went.
#[derive(Clone, Default, PartialEq, Eq)]
enum Ready {
    #[default]
    Pending,
    Open,
    /// The account policy refused the native account: no input is offered.
    Refused(String),
    Failed,
}

/// Why a native request got no result.
enum Rpc {
    /// The app-server answered with an error and changed nothing.
    Refused(String),
    /// The app-server is gone.
    Closed,
}

enum Opening {
    Refused(String),
    Failed,
}

impl From<Rpc> for Opening {
    fn from(_: Rpc) -> Self {
        Self::Failed
    }
}

struct Queued {
    id: String,
    prompt: String,
    plan: Option<(String, Asked)>,
}

#[derive(Clone)]
struct Asked {
    id: Value,
    method: String,
    kind: DecisionKind,
    epoch: u64,
    payload: Value,
    tool: Option<String>,
}

#[derive(Clone)]
struct AuthIdentity {
    method: String,
    email: Option<String>,
    workspace: Option<String>,
}

#[derive(Clone)]
struct Child {
    activation: String,
    status: String,
    pending: bool,
    ended_status: Option<String>,
    report: Option<String>,
}

/// What the session does next, in the order the real session would.
enum Work {
    /// The opening ended: a refusal with no input queued is announced.
    Opened,
    Pump,
    CheckChild(String),
    /// The session answered the app-server's request `id` with `result`.
    Reply { id: Value, result: Value, key: String, epoch: u64 },
    /// The app-server exited.
    Exit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Only what needs no native round trip: what the real session does before the receipt.
    Soon,
    All,
}

/// The Codex session of one host and the app-server behind it, with the persistent host's own
/// state of it.
#[derive(Default)]
pub struct Session {
    /// The doubled host, which says whether the app-server it started runs.
    process: Option<Served>,
    opened: bool,
    /// The session ended: the host takes no command any more.
    unavailable: bool,
    native: Option<Native>,
    ready: Ready,
    thread: Option<String>,
    turn: Option<String>,
    starting: bool,
    held: Vec<Value>,
    queue: VecDeque<Queued>,
    inputs: BTreeSet<String>,
    asked: BTreeMap<String, Asked>,
    answered: BTreeMap<String, String>,
    answering_plans: BTreeSet<String>,
    mode: Mode,
    model: Option<String>,
    auth: Option<AuthIdentity>,
    workspace_witness: Option<String>,
    children: BTreeSet<String>,
    child_states: BTreeMap<String, Child>,
    child_items: BTreeSet<String>,
    work: VecDeque<Work>,
    sequence: u64,
    closing: bool,
    closed: bool,
    failure: Option<String>,
}

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|text| !text.is_empty()).map(str::to_owned)
}

fn sha256(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn request_key(id: &Value) -> String {
    match id {
        Value::String(id) => format!("string:{id}"),
        other => format!("number:{other}"),
    }
}

fn rejected(code: &str, message: &str) -> Value {
    json!({"status": "rejected", "code": code, "message": message})
}

fn applied() -> Value {
    json!({"status": "accepted", "phase": "applied"})
}

/// The user's own private file at `path`, read only when it is a small regular file nobody else
/// can read; none when it does not exist.
fn private_json(path: &Path) -> Result<Option<Map<String, Value>>, Opening> {
    let invalid = || Opening::Refused("native_auth_policy_invalid".into());
    let meta = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(invalid()),
        Ok(meta) => meta,
    };
    if !meta.is_file() || meta.nlink() != 1 || meta.mode() & 0o777 != 0o600 || meta.len() > 4096 {
        return Err(invalid());
    }
    let bytes = std::fs::read(path).map_err(|_| invalid())?;
    serde_json::from_slice::<Value>(&bytes).map(|value| Some(object(&value))).map_err(|_| invalid())
}

impl Session {
    fn settings(bootstrap: &Bootstrap) -> Map<String, Value> {
        bootstrap.worker.as_ref().map(|worker| object(worker.settings.get("codex").unwrap_or(&Value::Null))).unwrap_or_default()
    }

    fn cwd(bootstrap: &Bootstrap) -> String {
        bootstrap.native.cwd.display().to_string()
    }

    fn command(&mut self, out: &mut Out, command: &Command) {
        if self.unavailable {
            return out.receipt(command, rejected("host_reconciliation_required", "Prior native work and effects need positive reconciliation"));
        }
        if let Some(required) = command.action.capability()
            && !CAPABILITIES.contains(&required)
        {
            let name = serde_json::to_value(&command.action).ok().and_then(|action| action["type"].as_str().map(str::to_owned)).unwrap_or_default();
            return out.receipt(command, rejected("unsupported", &format!("This host does not support {name}")));
        }
        let bootstrap = out.bootstrap();
        match command.action.clone() {
            Action::Start { cwd, input_id, prompt } => {
                if self.opened || cwd != Self::cwd(bootstrap) || bootstrap.journal_base.is_some() {
                    return out.troubled("the daemon started a session the host could not start".into());
                }
                self.open(out);
                self.offered(out, command, input_id.to_string(), prompt);
            }
            Action::Offer { input_id, prompt } => {
                if !self.opened && bootstrap.journal_base.is_some() {
                    self.open(out);
                }
                if !self.opened {
                    return out.troubled("the daemon offered an input to a host with no session".into());
                }
                self.offered(out, command, input_id.to_string(), prompt);
            }
            Action::Respond { request_id, epoch, kind, tool_use_id, decision, asked } => {
                let answered = self.opened && self.respond(out, &request_id.0, epoch, kind, tool_use_id, &decision, &Value::Object(asked));
                out.receipt(command, if answered { applied() } else { rejected("stale_request", "Claude is not waiting for this request") });
                self.drive(out, Phase::All);
            }
            Action::Interrupt => {
                if !self.opened {
                    return out.troubled("the daemon interrupted a host with no session".into());
                }
                self.interrupt(out);
                out.receipt(command, applied());
                self.drive(out, Phase::All);
            }
            Action::Close => {
                if !self.opened {
                    return out.troubled("the daemon closed a host with no session".into());
                }
                self.close(out);
                self.drive(out, Phase::All);
                out.receipt(command, applied());
            }
            Action::ForceStop => {
                if !self.controlled(out, command) {
                    return;
                }
                self.force_stop(out);
                self.drive(out, Phase::All);
                out.receipt(command, applied());
            }
            Action::SetModel { model } => {
                if !self.controlled(out, command) {
                    return;
                }
                let offered = self.supported_models(out);
                if !offered.contains(&model) {
                    out.receipt(command, rejected("unsupported_model", "Claude does not offer this model"));
                } else {
                    self.model = Some(model);
                    out.receipt(command, applied());
                }
                self.drive(out, Phase::All);
            }
            Action::SetPermissionMode { mode } => {
                if !self.controlled(out, command) {
                    return;
                }
                self.mode = match mode {
                    protocol::PermissionMode::Default => Mode::Default,
                    protocol::PermissionMode::Plan => Mode::Plan,
                    protocol::PermissionMode::DontAsk => Mode::DontAsk,
                };
                out.receipt(command, applied());
                self.drive(out, Phase::All);
            }
            Action::McpStatus => {
                if !self.controlled(out, command) {
                    return;
                }
                let thread = self.thread.clone();
                let servers = match self.rpc(out, "mcpServerStatus/list", json!({"threadId": thread, "cursor": null, "limit": 100, "detail": "full"})) {
                    Ok(page) => page["data"].as_array().cloned().unwrap_or_default().iter().filter_map(|server| {
                        let name = text(&server["name"])?;
                        let status = text(&server["runtimeStatus"]).or_else(|| text(&server["authStatus"])).unwrap_or_else(|| "unknown".into());
                        let mut listed = json!({"name": name, "status": status});
                        if let Some(error) = text(&server["toolsError"]) {
                            listed["error"] = json!(error);
                        }
                        Some(listed)
                    }).collect::<Vec<_>>(),
                    Err(_) => return out.troubled("the app-server never answered the MCP status".into()),
                };
                out.observe(json!({"type": "sdk", "kind": "control_result", "native": {}, "payload": {"control": "mcp_status", "command_id": command.command_id.to_string(), "servers": servers}, "failures": []}));
                out.receipt(command, applied());
                self.drive(out, Phase::All);
            }
            Action::McpAuthenticate { server } => {
                if !self.controlled(out, command) {
                    return;
                }
                let thread = self.thread.clone();
                let sign_in = match self.rpc(out, "mcpServer/oauth/login", json!({"threadId": thread, "name": server})) {
                    Ok(result) => match text(&result["authorizationUrl"]).filter(|url| url.starts_with("https://") || url.starts_with("http://localhost") || url.starts_with("http://127.0.0.1")) {
                        Some(url) => json!({"outcome": "browser", "url": url}),
                        None => json!({"outcome": "refused", "error": "Claude gave no sign-in address Alondra can open"}),
                    },
                    Err(_) => json!({"outcome": "refused", "error": "Codex RPC failed"}),
                };
                out.observe(json!({"type": "sdk", "kind": "control_result", "native": {}, "payload": {"control": "mcp_authenticate", "command_id": command.command_id.to_string(), "server": server, "sign_in": sign_in}, "failures": []}));
                out.receipt(command, applied());
                self.drive(out, Phase::All);
            }
            Action::McpReconnect { .. } | Action::McpClearAuth { .. } => out.troubled("the daemon sent a control the Codex host does not announce".into()),
        }
    }

    /// A control opens the released session of a host that resumes one; with no session it is
    /// refused.
    fn controlled(&mut self, out: &mut Out, command: &Command) -> bool {
        if !self.opened && out.bootstrap().opens_on_control {
            self.open(out);
            self.drive(out, Phase::Soon);
        }
        if !self.opened {
            out.receipt(command, rejected("session_absent", "No Claude session is running on this host"));
        }
        self.opened
    }

    /// The host offers an input: the receipt says it was offered once the session's opening let it
    /// be, or why it was refused before reaching the app-server.
    fn offered(&mut self, out: &mut Out, command: &Command, input: String, prompt: String) {
        if self.closed || self.closing {
            let code = self.failure.clone().unwrap_or_else(|| "native_session_ended".into());
            return out.receipt(command, rejected(&code, "The native session refused this input before submission"));
        }
        if self.inputs.contains(&input) || self.queue.len() >= MAX_QUEUED || prompt.len() > MAX_PROMPT {
            return out.troubled("the session refused an input without saying why".into());
        }
        self.inputs.insert(input.clone());
        self.queue.push_back(Queued { id: input, prompt, plan: None });
        self.work.push_back(Work::Pump);
        let waits_for_opening = out.bootstrap().worker.is_some() && self.thread.is_none();
        if waits_for_opening {
            self.drive(out, Phase::Soon);
            let refusal = match &self.ready {
                Ready::Refused(code) => Some(code.clone()),
                Ready::Failed => Some(self.failure.clone().filter(|failure| failure == "native_initialization_timeout").unwrap_or_else(|| "native_initialization_failed".into())),
                Ready::Pending | Ready::Open => None,
            };
            if let Some(code) = refusal {
                out.receipt(command, rejected(&code, "The native session refused this input before submission"));
                return self.drive(out, Phase::All);
            }
        }
        out.receipt(command, json!({"status": "accepted", "phase": "offered"}));
        self.drive(out, Phase::All);
    }

    /// The session starts its app-server and opens its thread.
    fn open(&mut self, out: &mut Out) {
        self.opened = true;
        let bootstrap = out.bootstrap();
        self.model = bootstrap.worker.as_ref().and_then(|worker| worker.model.clone());
        if Self::settings(bootstrap).get("initial_mode").and_then(Value::as_str) == Some("plan") {
            self.mode = Mode::Plan;
        }
        let native = Native::new(&bootstrap.native.executable, &bootstrap.native.cwd, &bootstrap.native.env, out.host.process_id);
        if let Some(unplayed) = native.unplayed() {
            out.troubled(unplayed);
        }
        self.native = Some(native);
        if let Some(process) = &self.process {
            process.set_native(true);
        }
        self.ready = match self.initialize(out) {
            Ok(()) => Ready::Open,
            Err(Opening::Refused(code)) => Ready::Refused(code),
            Err(Opening::Failed) => Ready::Failed,
        };
        self.work.push_back(Work::Opened);
    }

    fn initialize(&mut self, out: &mut Out) -> Result<(), Opening> {
        let release = out.bootstrap().manifest.host_release.clone();
        self.rpc(out, "initialize", json!({"clientInfo": {"name": "alondra", "title": "Alondra", "version": release}, "capabilities": {"experimentalApi": true}}))?;
        self.notify(out, json!({"method": "initialized"}));
        self.verify_account(out)?;
        let bootstrap = out.bootstrap();
        let settings = Self::settings(bootstrap);
        let mut overrides = object(settings.get("config").unwrap_or(&Value::Null));
        overrides.insert("features.goals".into(), json!(false));
        let role = Self::role_environment(bootstrap);
        let mut servers = object(settings.get("mcp_servers").unwrap_or(&Value::Null));
        if bootstrap.worker.is_some() && !servers.keys().any(|name| name == "alondra" || name == "alondra-orchestrator") {
            return Err(Opening::Failed);
        }
        for (name, server) in &mut servers {
            if name == "alondra" || name == "alondra-orchestrator" {
                let mut env: Map<String, Value> = object(&server["env"]).into_iter().filter(|(key, _)| !key.starts_with("ALONDRA_")).collect();
                env.extend(role.clone());
                let mut kept = object(server);
                kept.insert("env".into(), Value::Object(env));
                kept.insert("env_vars".into(), json!([]));
                kept.insert("default_tools_approval_mode".into(), json!("approve"));
                *server = Value::Object(kept);
            }
            let mut enabled = object(server);
            enabled.insert("enabled".into(), json!(true));
            enabled.insert("required".into(), json!(true));
            overrides.insert(format!("mcp_servers.{name}"), Value::Object(enabled));
        }
        let mut args = json!({"cwd": Self::cwd(bootstrap), "config": overrides, "approvalPolicy": "on-request", "approvalsReviewer": "auto_review", "sandbox": "workspace-write"});
        if let Some(model) = &self.model {
            args["model"] = json!(model);
        }
        if let Some(instructions) = settings.get("developerInstructions").and_then(text) {
            args["developerInstructions"] = json!(instructions);
        }
        let existing = self.read_mapping(bootstrap)?;
        if bootstrap.resumes() && existing.is_none() {
            return Err(Opening::Failed);
        }
        let result = match &existing {
            Some(thread) => {
                args["threadId"] = json!(thread);
                args["excludeTurns"] = json!(true);
                self.rpc(out, "thread/resume", args)?
            }
            None => {
                args["ephemeral"] = json!(false);
                args["environments"] = Value::Null;
                self.rpc(out, "thread/start", args)?
            }
        };
        if result["modelProvider"] != "openai" {
            return Err(Opening::Refused("native_model_provider_unsupported".into()));
        }
        let thread = text(&result["thread"]["id"]).filter(|id| existing.as_ref().is_none_or(|existing| existing == id)).ok_or(Opening::Failed)?;
        self.thread = Some(thread.clone());
        if self.model.is_none() {
            self.model = text(&result["model"]);
        }
        self.write_mapping(out.bootstrap(), &thread).map_err(|_| Opening::Failed)?;
        let bootstrap = out.bootstrap();
        let mut ready = json!({"threadId": thread, "logicalConversationId": bootstrap.native.session_id, "cwd": Self::cwd(bootstrap), "processGeneration": bootstrap.host_instance_id});
        if let Some(model) = &self.model {
            ready["model"] = json!(model);
        }
        self.emit(out, "alondra/threadReady", ready, "initialized");
        self.work.push_back(Work::Pump);
        Ok(())
    }

    fn role_environment(bootstrap: &Bootstrap) -> Map<String, Value> {
        let env = &bootstrap.native.env;
        let mut role: Map<String, Value> = ROLE_ENVIRONMENT.iter().filter_map(|key| env.get(*key).map(|value| ((*key).to_owned(), json!(value)))).collect();
        if let Some(parent) = bootstrap.state_dir.parent().filter(|_| !bootstrap.state_dir.as_os_str().is_empty()) {
            role.insert("ALONDRA_CODEX_THREAD_MAPPING".into(), json!(parent.join("native-threads").join(&bootstrap.native.session_id)));
        }
        role
    }

    fn mapping(bootstrap: &Bootstrap) -> Option<PathBuf> {
        let parent = bootstrap.state_dir.parent().filter(|_| !bootstrap.state_dir.as_os_str().is_empty())?;
        Some(parent.join("native-threads").join(&bootstrap.native.session_id))
    }

    fn home_digest(bootstrap: &Bootstrap) -> String {
        sha256(bootstrap.native.env.get("CODEX_HOME").map_or("native_default", String::as_str).as_bytes())
    }

    fn read_mapping(&self, bootstrap: &Bootstrap) -> Result<Option<String>, Opening> {
        let Some(mapping) = Self::mapping(bootstrap) else { return Ok(None) };
        if let Some(folder) = mapping.parent() {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder).map_err(|_| Opening::Failed)?;
        }
        let bytes = match std::fs::read(&mapping) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Opening::Failed),
            Ok(bytes) => bytes,
        };
        let retained: Value = serde_json::from_slice(&bytes).map_err(|_| Opening::Failed)?;
        if retained["accountId"] != json!(bootstrap.fence.account_id) || retained["homeDigest"] != json!(Self::home_digest(bootstrap)) {
            return Err(Opening::Failed);
        }
        text(&retained["threadId"]).filter(|id| id.len() <= 256).map(Some).ok_or(Opening::Failed)
    }

    fn write_mapping(&self, bootstrap: &Bootstrap, thread: &str) -> io::Result<()> {
        let Some(mapping) = Self::mapping(bootstrap) else { return Ok(()) };
        let instructions = Self::settings(bootstrap).get("developerInstructions").and_then(Value::as_str).unwrap_or_default().to_owned();
        let record = json!({"threadId": thread, "accountId": bootstrap.fence.account_id, "homeDigest": Self::home_digest(bootstrap), "instructionsDigest": sha256(instructions.as_bytes())});
        let next = PathBuf::from(format!("{}.next", mapping.display()));
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&next).and_then(|mut file| io::Write::write_all(&mut file, record.to_string().as_bytes()))?;
        std::fs::rename(next, mapping)
    }

    fn policy(bootstrap: &Bootstrap) -> Result<Map<String, Value>, Opening> {
        let mut policy = object(Self::settings(bootstrap).get("nativeAccountPolicy").unwrap_or(&Value::Null));
        if bootstrap.worker.is_some() && !bootstrap.state_dir.as_os_str().is_empty() {
            let original = private_json(&bootstrap.state_dir.join("native-account.json"))?;
            if let Some(original) = original {
                if let Some(email) = original.get("email").and_then(Value::as_str).map(str::trim).filter(|email| !email.is_empty()) {
                    policy.insert("expectedEmail".into(), json!(email));
                }
                if let Some(identity) = original.get("native_identity").and_then(text) {
                    policy.insert("expectedWorkspaceId".into(), json!(identity));
                }
            }
        }
        Ok(policy)
    }

    fn verify_account(&mut self, out: &mut Out) -> Result<(), Opening> {
        match self.check_account(out) {
            Err(Opening::Failed) => Err(Opening::Refused("native_auth_unavailable".into())),
            checked => checked,
        }
    }

    fn check_account(&mut self, out: &mut Out) -> Result<(), Opening> {
        let refused = |code: &str| Opening::Refused(code.into());
        let bootstrap = out.bootstrap();
        let policy = Self::policy(bootstrap)?;
        let cwd = Self::cwd(bootstrap);
        let effective = self.rpc(out, "config/read", json!({"cwd": cwd, "includeLayers": false}))?;
        if text(&effective["config"]["model_provider"]).unwrap_or_else(|| "openai".into()) != "openai" {
            return Err(refused("native_model_provider_unsupported"));
        }
        let status = self.rpc(out, "getAuthStatus", json!({"includeToken": false, "refreshToken": false}))?;
        let read = self.rpc(out, "account/read", json!({"refreshToken": false}))?;
        if status["requiresOpenaiAuth"] != true || read["requiresOpenaiAuth"] != true {
            return Err(refused("native_provider_auth_unsupported"));
        }
        let method = text(&status["authMethod"]).unwrap_or_default();
        let account = object(&read["account"]);
        let mut identity = AuthIdentity { method: method.clone(), email: None, workspace: None };
        match method.as_str() {
            "apikey" => {
                let account_id = out.bootstrap().fence.account_id.clone();
                if account.get("type").and_then(Value::as_str) != Some("apiKey") || policy.get("paidAccountId").and_then(Value::as_str) != Some(account_id.as_str()) {
                    return Err(refused("paid_authorization_required"));
                }
            }
            "chatgpt" => {
                if account.get("type").and_then(Value::as_str) != Some("chatgpt") {
                    return Err(refused("native_auth_mode_unsupported"));
                }
                let email = account.get("email").and_then(text).map(|email| email.trim().to_lowercase()).filter(|email| !email.is_empty()).ok_or_else(|| refused("native_account_identity_unavailable"))?;
                if email.len() > 1024 {
                    return Err(refused("native_account_identity_invalid"));
                }
                let usage = self.rpc(out, "account/rateLimits/read", json!({})).unwrap_or_else(|_| json!({}));
                let workspace = usage["ordinaryUsageAllowed"].is_boolean().then(|| text(&usage["accountId"])).flatten();
                if workspace.as_ref().is_some_and(|workspace| workspace.len() > 1024 || workspace.trim() != workspace || workspace.chars().any(char::is_control)) {
                    return Err(refused("native_account_identity_invalid"));
                }
                if policy.get("expectedEmail").and_then(Value::as_str).map(|expected| expected.trim().to_lowercase()).is_some_and(|expected| !expected.is_empty() && expected != email) {
                    return Err(refused("native_account_identity_changed"));
                }
                if let (Some(expected), Some(workspace)) = (policy.get("expectedWorkspaceId").and_then(text), &workspace)
                    && &expected != workspace
                {
                    return Err(refused("native_account_identity_changed"));
                }
                identity.email = Some(email);
                identity.workspace = workspace;
            }
            _ => return Err(refused("native_auth_mode_unsupported")),
        }
        self.seal_account(out.bootstrap(), identity)
    }

    fn seal_account(&mut self, bootstrap: &Bootstrap, identity: AuthIdentity) -> Result<(), Opening> {
        let changed = || Opening::Refused("native_account_identity_changed".into());
        if let Some(workspace) = &identity.workspace {
            if self.workspace_witness.as_ref().is_some_and(|witness| witness != workspace) {
                return Err(changed());
            }
            self.workspace_witness = Some(workspace.clone());
        }
        let matches = |method: &str, email: &Option<String>, workspace: &Option<String>| {
            method == identity.method && *email == identity.email && (workspace.is_none() || identity.workspace.is_none() || *workspace == identity.workspace)
        };
        if let Some(sealed) = &self.auth {
            return if matches(&sealed.method, &sealed.email, &sealed.workspace) { Ok(()) } else { Err(changed()) };
        }
        let mut original = identity.clone();
        if !bootstrap.state_dir.as_os_str().is_empty() {
            let file = bootstrap.state_dir.join("native-auth-account.json");
            match private_json(&file)? {
                Some(prior) => {
                    let method = prior.get("authMethod").and_then(Value::as_str).unwrap_or_default().to_owned();
                    let email = prior.get("email").and_then(Value::as_str).map(str::to_owned);
                    let workspace = prior.get("workspaceId").and_then(text);
                    if !matches(&method, &email, &workspace) {
                        return Err(changed());
                    }
                    if identity.method == "apikey" {
                        return Err(Opening::Refused("native_api_key_resume_unverifiable".into()));
                    }
                    original.workspace = workspace;
                    if self.workspace_witness.is_none() {
                        self.workspace_witness = original.workspace.clone();
                    }
                }
                None => {
                    if bootstrap.resumes() {
                        return Err(Opening::Refused("native_account_identity_unavailable".into()));
                    }
                    let record = json!({"authMethod": identity.method, "email": identity.email, "workspaceId": identity.workspace});
                    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file)
                        .and_then(|mut written| io::Write::write_all(&mut written, record.to_string().as_bytes()))
                        .map_err(|_| Opening::Refused("native_auth_policy_invalid".into()))?;
                    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
        self.auth = Some(original);
        Ok(())
    }

    /// Sends the app-server a message that wants no answer.
    fn notify(&mut self, out: &mut Out, message: Value) {
        if let Some(native) = self.native.as_mut().filter(|_| !self.closed) {
            let printed = native.read(&message);
            self.printed(out, printed, None);
        }
    }

    /// A request to the app-server, and what it answered.
    fn rpc(&mut self, out: &mut Out, method: &str, params: Value) -> Result<Value, Rpc> {
        if self.closed {
            return Err(Rpc::Closed);
        }
        let Some(native) = self.native.as_mut() else { return Err(Rpc::Closed) };
        self.sequence += 1;
        let id = json!(format!("alondra:{}", self.sequence));
        let printed = native.read(&json!({"id": id, "method": method, "params": params}));
        match self.printed(out, printed, Some(&id)) {
            Some(response) => match response.get("error") {
                Some(error) => Err(Rpc::Refused(error["code"].as_i64().map_or_else(|| "failed".into(), |code| code.to_string()))),
                None => Ok(response.get("result").cloned().map(|result| Value::Object(object(&result))).unwrap_or_else(|| json!({}))),
            },
            None => {
                out.troubled(format!("the app-server never answered {method}"));
                Err(Rpc::Closed)
            }
        }
    }

    /// Reads what the app-server printed, in order, and returns the response to `awaited`.
    fn printed(&mut self, out: &mut Out, printed: Printed, awaited: Option<&Value>) -> Option<Value> {
        let mut response = None;
        for message in printed {
            let has_id = message.get("id").is_some_and(|id| id.is_string() || id.is_number());
            if message["method"].is_string() {
                if has_id {
                    self.request(out, &message);
                } else if self.starting {
                    self.held.push(message);
                } else {
                    self.notification(out, &message);
                }
            } else if has_id && awaited == message.get("id") {
                response = Some(message);
            }
        }
        response
    }

    fn emit(&mut self, out: &mut Out, method: &str, params: Value, kind: &str) -> u64 {
        let native = match &self.turn {
            Some(turn) => json!({"turn_id": turn}),
            None => json!({}),
        };
        out.observe(json!({"type": "sdk", "kind": kind, "native": native, "payload": {"method": method, "params": params}, "failures": []}))
    }

    fn release_held(&mut self, out: &mut Out) {
        for message in std::mem::take(&mut self.held) {
            self.notification(out, &message);
        }
    }

    /// Runs what the session does next, until nothing is left or, in `Phase::Soon`, until what is
    /// next needs the app-server.
    fn drive(&mut self, out: &mut Out, phase: Phase) {
        while let Some(work) = self.work.front() {
            let later = match work {
                Work::Opened => false,
                Work::Pump => matches!(self.ready, Ready::Open) && !self.closed && !self.closing && self.turn.is_none() && !self.queue.is_empty(),
                Work::CheckChild(_) | Work::Reply { .. } | Work::Exit => true,
            };
            if later && phase == Phase::Soon {
                return;
            }
            let Some(work) = self.work.pop_front() else { return };
            match work {
                Work::Opened => {
                    if let Ready::Refused(code) = self.ready.clone() {
                        if self.queue.is_empty() {
                            self.emit(out, "alondra/accountBlocked", json!({"code": code}), "diagnostic");
                        }
                    } else if self.ready == Ready::Failed {
                        self.fail("native_initialization_failed");
                    }
                }
                Work::Pump => self.pump(out),
                Work::CheckChild(child) => self.check_child(out, &child),
                Work::Reply { id, result, key, epoch } => {
                    let printed = self.native.as_mut().filter(|_| !self.closed).map(|native| native.read(&json!({"id": id, "result": result})));
                    self.settled(out, &key, epoch, "answered");
                    if let Some(printed) = printed {
                        self.printed(out, printed, None);
                    }
                }
                Work::Exit => self.exited(out),
            }
        }
    }

    fn fail(&mut self, code: &str) {
        if self.closed || self.closing {
            return;
        }
        self.failure = Some(code.into());
        self.begin_close();
    }

    fn begin_close(&mut self) {
        if self.closing || self.closed {
            return;
        }
        self.closing = true;
        self.queue.clear();
        self.work.push_back(Work::Exit);
    }

    fn close(&mut self, out: &mut Out) {
        self.begin_close();
        self.drive(out, Phase::All);
    }

    /// The app-server exited: every decision it waited on is cancelled, the host says how it ended,
    /// and the host ends once the daemon read that.
    fn exited(&mut self, out: &mut Out) {
        if self.closed {
            return;
        }
        if !self.closing {
            self.failure.get_or_insert_with(|| "native_process_exited".into());
        }
        self.closing = true;
        self.queue.clear();
        self.closed = true;
        self.native = None;
        if let Some(process) = &self.process {
            process.set_native(false);
        }
        for (key, asked) in std::mem::take(&mut self.asked) {
            out.observe(json!({"type": "decision_settled", "request_id": key, "epoch": asked.epoch, "outcome": "cancelled"}));
        }
        if let Some(failure) = self.failure.clone() {
            self.emit(out, "alondra/transportFailed", json!({"code": failure}), "diagnostic");
        }
        out.observe(json!({"type": "process_exited", "exit_code": null, "signal": "SIGTERM"}));
        self.unavailable = true;
        out.wire.closing = true;
    }

    /// The session hands the app-server the next input as a turn, once its account is checked.
    fn pump(&mut self, out: &mut Out) {
        if self.closed || self.closing || self.turn.is_some() || self.queue.is_empty() {
            return;
        }
        let checked = match self.ready.clone() {
            Ready::Open => self.verify_account(out),
            Ready::Refused(code) => Err(Opening::Refused(code)),
            Ready::Failed | Ready::Pending => Err(Opening::Failed),
        };
        let Some(front) = self.queue.front().map(|input| input.id.clone()) else { return };
        match checked {
            Err(Opening::Refused(code)) => return self.refused(out, &front, &code),
            Err(Opening::Failed) => return self.fail("native_turn_start_uncertain"),
            Ok(()) => {}
        }
        if self.turn.is_some() || self.closed || self.closing {
            return;
        }
        let Some(input) = self.queue.pop_front() else { return };
        self.starting = true;
        let thread = self.thread.clone();
        self.emit(out, "alondra/inputStarting", json!({"threadId": thread, "inputId": input.id}), "message");
        let bootstrap = out.bootstrap();
        let mut params = self.policy_for(bootstrap);
        params["threadId"] = json!(thread);
        params["clientUserMessageId"] = json!(input.id);
        params["input"] = json!([{"type": "text", "text": input.prompt, "text_elements": []}]);
        match self.rpc(out, "turn/start", params) {
            Ok(response) => {
                let Some(turn) = text(&response["turn"]["id"]) else { return self.fail("native_turn_start_uncertain") };
                self.turn = Some(turn.clone());
                match &input.plan {
                    Some((key, entry)) => {
                        self.asked.remove(key);
                        self.answering_plans.remove(key);
                        self.emit(out, "alondra/planContinuationAccepted", json!({"threadId": thread, "turnId": turn, "inputId": input.id, "requestId": key, "epoch": entry.epoch}), "message");
                    }
                    None => {
                        let mut accepted = json!({"threadId": thread, "turnId": turn, "inputId": input.id});
                        if let Some(model) = &self.model {
                            accepted["model"] = json!(model);
                        }
                        self.emit(out, "alondra/inputAccepted", accepted, "message");
                    }
                }
                self.starting = false;
                self.release_held(out);
            }
            Err(Rpc::Refused(code)) if input.plan.is_none() => {
                self.starting = false;
                self.emit(out, "alondra/inputRejected", json!({"threadId": thread, "inputId": input.id, "code": code}), "message");
                self.release_held(out);
            }
            Err(_) => self.fail("native_turn_start_uncertain"),
        }
        if self.turn.is_none() && !self.queue.is_empty() && !self.closed && !self.closing {
            self.work.push_back(Work::Pump);
        }
    }

    /// The account policy refused the input at the front of the queue before the app-server got it.
    fn refused(&mut self, out: &mut Out, input: &str, code: &str) {
        let thread = self.thread.clone();
        if let Some(refused) = self.queue.pop_front().filter(|front| front.id == input) {
            match refused.plan {
                Some((key, entry)) => {
                    self.answering_plans.remove(&key);
                    self.settled(out, &key, entry.epoch, "cancelled");
                }
                None => {
                    self.emit(out, "alondra/inputRefused", json!({"threadId": thread, "inputId": input, "nativeSubmitted": false, "code": code}), "diagnostic");
                }
            }
        }
        self.emit(out, "alondra/accountBlocked", json!({"threadId": thread, "code": code}), "diagnostic");
        if !self.queue.is_empty() {
            self.work.push_back(Work::Pump);
        }
    }

    /// What each turn is started with: the thread's working folder, model, effort and the policy
    /// of the session's mode.
    fn policy_for(&self, bootstrap: &Bootstrap) -> Value {
        let settings = Self::settings(bootstrap);
        let worker = bootstrap.worker.as_ref();
        let effort = settings.get("reasoning_effort").and_then(text).or_else(|| worker.and_then(|worker| worker.effort.clone()));
        let chosen = self.model.clone().or_else(|| worker.and_then(|worker| worker.model.clone()));
        let cwd = Self::cwd(bootstrap);
        let never = self.mode == Mode::DontAsk;
        let sandbox = if self.mode == Mode::Plan {
            json!({"type": "readOnly"})
        } else {
            json!({"type": "workspaceWrite", "writableRoots": [cwd], "networkAccess": false, "excludeTmpdirEnvVar": true, "excludeSlashTmp": true})
        };
        let mut policy = json!({"cwd": cwd, "approvalPolicy": if never { "never" } else { "on-request" }, "approvalsReviewer": if never { "user" } else { "auto_review" }, "sandboxPolicy": sandbox});
        if let Some(model) = &chosen {
            policy["model"] = json!(model);
            let mode = if self.mode == Mode::Plan { "plan" } else { "default" };
            policy["collaborationMode"] = json!({"mode": mode, "settings": {"model": model, "reasoning_effort": effort, "developer_instructions": null}});
        }
        if let Some(effort) = effort {
            policy["effort"] = json!(effort);
        }
        policy
    }

    fn notification(&mut self, out: &mut Out, message: &Value) {
        out.observe(json!({"type": "sdk", "kind": "message", "native": {}, "payload": message, "failures": []}));
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        let native_thread = text(&params["threadId"]);
        let ours = native_thread.is_some() && native_thread == self.thread;
        let of_child = native_thread.as_ref().is_some_and(|thread| self.children.contains(thread));
        if let Some(thread) = &native_thread
            && (ours || of_child)
            && matches!(method, "item/started" | "item/completed")
        {
            self.child_activity(&params["item"], method == "item/started", thread);
        }
        if of_child && method == "item/completed"
            && let Some(child) = native_thread.clone()
        {
            self.work.push_back(Work::CheckChild(child));
        }
        if method == "item/completed" && ours {
            let item = &params["item"];
            if item["type"] == "plan"
                && let (Some(id), Some(plan)) = (item["id"].as_str(), item["text"].as_str())
            {
                let key = format!("plan:{}:{id}", out.bootstrap().host_instance_id);
                if !self.asked.contains_key(&key) && !self.answered.contains_key(&key) {
                    let payload = json!({"provider": "codex", "method": "alondra/planApproval", "native": params, "tool": "plan", "input": {"plan": plan}});
                    let epoch = out.observe(json!({"type": "decision_requested", "request_id": key, "tool_use_id": id, "kind": "plan", "payload": payload}));
                    self.asked.insert(key.clone(), Asked { id: json!(key), method: "alondra/planApproval".into(), kind: DecisionKind::Plan, epoch, payload, tool: Some(id.to_owned()) });
                }
            }
        }
        if method == "turn/started" && ours && let Some(turn) = text(&params["turn"]["id"]) {
            self.turn = Some(turn);
        }
        if method == "turn/completed" && ours && params["turn"]["id"].as_str().is_some() && text(&params["turn"]["id"]) == self.turn {
            self.turn = None;
            self.work.push_back(Work::Pump);
        }
    }

    fn child_activity(&mut self, item: &Value, started: bool, parent: &str) {
        let Some(id) = text(&item["id"]) else { return };
        let kind = item["type"].as_str().unwrap_or_default();
        if !matches!(kind, "collabAgentToolCall" | "subAgentActivity") {
            return;
        }
        let key = format!("{parent}:{}:{id}", if started { "started" } else { "completed" });
        if !self.child_items.insert(key) {
            return;
        }
        if kind == "collabAgentToolCall" {
            let activating = matches!(item["tool"].as_str(), Some("spawnAgent" | "sendInput" | "resumeAgent" | "followupTask"));
            let states = object(&item["agentsStates"]);
            for child in item["receiverThreadIds"].as_array().cloned().unwrap_or_default() {
                let Some(child) = text(&child) else { continue };
                if Some(&child) == self.thread.as_ref() || child == parent || (!self.children.contains(&child) && !activating) {
                    continue;
                }
                self.children.insert(child.clone());
                let old = self.child_states.get(&child).cloned();
                let state = object(states.get(&child).unwrap_or(&Value::Null));
                let status = state.get("status").and_then(text).unwrap_or_else(|| if started { "running".into() } else { old.as_ref().map_or_else(|| "unknown".into(), |old| old.status.clone()) });
                let activation = if activating { id.clone() } else { old.as_ref().map_or_else(|| id.clone(), |old| old.activation.clone()) };
                let report = state.get("message").and_then(text)
                    .or_else(|| old.as_ref().filter(|old| !activating || old.activation == id).and_then(|old| old.report.clone()));
                self.child_states.insert(child.clone(), Child { activation, status, pending: activating && started, ended_status: None, report });
                if !started {
                    self.work.push_back(Work::CheckChild(child));
                }
            }
        } else {
            let Some(child) = text(&item["agentThreadId"]) else { return };
            let Some(old) = self.child_states.get(&child).cloned() else { return };
            let active = matches!(item["kind"].as_str(), Some("started" | "interacted"));
            let status = match item["kind"].as_str() {
                _ if active => "running".to_owned(),
                Some("completed") => "completed".into(),
                Some("interrupted") => "interrupted".into(),
                _ => old.status.clone(),
            };
            let activation = if active { id } else { old.activation.clone() };
            let report = if active { None } else { old.report.clone() };
            self.child_states.insert(child.clone(), Child { activation, status, pending: false, report, ..old });
            if !active {
                self.work.push_back(Work::CheckChild(child));
            }
        }
    }

    /// Once a managed child ended with no turn running and no background terminal left, and every
    /// other child is quiet too, the session says each finished, with its report.
    fn check_child(&mut self, out: &mut Out, child: &str) {
        let Some(state) = self.child_states.get(child).cloned() else { return };
        if state.pending || !matches!(state.status.as_str(), "completed" | "errored" | "interrupted" | "shutdown" | "quiescent") || self.closed || self.closing {
            return;
        }
        let history = self.child_history(out, child);
        let Ok((active, report)) = history else {
            if !self.closed && !self.closing {
                let thread = self.thread.clone();
                self.emit(out, "alondra/childObservationUncertain", json!({"threadId": thread, "childThreadId": child}), "diagnostic");
            }
            return;
        };
        if active {
            return;
        }
        let report = report.or_else(|| state.report.clone());
        let Ok(terminals) = self.rpc(out, "thread/backgroundTerminals/list", json!({"threadId": child, "cursor": null, "limit": 100})) else { return };
        let current = self.child_states.get(child).cloned();
        if current.as_ref().is_none_or(|current| current.activation != state.activation || current.status != state.status || current.pending) || self.closed || self.closing {
            return;
        }
        if terminals["data"].as_array().is_some_and(|terminals| !terminals.is_empty()) {
            return;
        }
        let ended_status = state.ended_status.clone().or_else(|| Some(state.status.clone()));
        self.child_states.insert(child.to_owned(), Child { status: "quiescent".into(), ended_status, report: report.or(state.report.clone()), ..state });
        if self.child_states.values().any(|other| !matches!(other.status.as_str(), "quiescent" | "settled")) {
            return;
        }
        let thread = self.thread.clone();
        for (id, completed) in self.child_states.clone() {
            if completed.status != "quiescent" {
                continue;
            }
            let mut quiet = json!({"threadId": thread, "childThreadId": id, "activationKey": completed.activation, "status": completed.ended_status.clone().unwrap_or_else(|| "unknown".into())});
            if let Some(report) = &completed.report {
                quiet["report"] = json!(report);
            }
            self.emit(out, "alondra/childQuiescent", quiet, "message");
            self.child_states.insert(id, Child { status: "settled".into(), ..completed });
        }
    }

    fn child_history(&mut self, out: &mut Out, child: &str) -> Result<(bool, Option<String>), Rpc> {
        let latest = self.rpc(out, "thread/turns/list", json!({"threadId": child, "limit": 1, "sortDirection": "desc", "itemsView": "notLoaded"}))?;
        let turns = latest["data"].as_array().cloned().ok_or(Rpc::Closed)?;
        let turn = turns.first().cloned().unwrap_or(Value::Null);
        if !turns.is_empty() && !matches!(turn["status"].as_str(), Some("completed" | "interrupted" | "failed")) {
            return Ok((true, None));
        }
        let page = self.rpc(out, "thread/items/list", json!({"threadId": child, "cursor": null, "limit": 16, "sortDirection": "desc"}))?;
        let items: Vec<Value> = page["data"].as_array().cloned().ok_or(Rpc::Closed)?.iter().map(|entry| {
            let mut item = object(&entry["item"]);
            item.insert("native_turn_id".into(), entry["turnId"].clone());
            Value::Object(item)
        }).collect();
        let report = items.iter().find(|item| item["native_turn_id"] == turn["id"] && item["type"] == "agentMessage" && (item["phase"] == "final_answer" || item["phase"].is_null()) && item["text"].is_string())
            .and_then(|item| text(&item["text"]));
        for item in items.iter().rev() {
            self.child_activity(item, false, child);
        }
        Ok((false, report))
    }

    /// The app-server asks the user something: the host announces the decision once per request.
    fn request(&mut self, out: &mut Out, message: &Value) {
        let id = message["id"].clone();
        let method = message["method"].as_str().unwrap_or_default().to_owned();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let kind = match method.as_str() {
            "item/tool/requestUserInput" => DecisionKind::Question,
            "mcpServer/elicitation/request" => DecisionKind::Elicitation,
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" | "item/permissions/requestApproval" => DecisionKind::Permission,
            _ => {
                self.notify(out, json!({"id": id, "error": {"code": -32601, "message": "This Alondra host does not expose that request"}}));
                return;
            }
        };
        let key = request_key(&id);
        let identity = sha256(json!({"method": method, "params": params}).to_string().as_bytes());
        if let Some(answered) = self.answered.get(&key) {
            if *answered != identity {
                self.fail("native_answered_request_identity_conflict");
            }
            return;
        }
        if let Some(previous) = self.asked.get(&key) {
            if previous.method != method || previous.payload["native"] != params {
                self.fail("native_request_identity_conflict");
            }
            return;
        }
        let tool = text(&params["itemId"]);
        let display = match method.as_str() {
            "item/commandExecution/requestApproval" => "exec_command",
            "item/fileChange/requestApproval" => "apply_patch",
            "item/permissions/requestApproval" => "request_permissions",
            _ => "request_user_input",
        };
        let input = if kind == DecisionKind::Question {
            json!({"questions": params["questions"]})
        } else {
            let mut input = object(&params);
            if let Some(reason) = params.get("reason") {
                input.insert("description".into(), reason.clone());
            }
            Value::Object(input)
        };
        let mut payload = json!({"provider": "codex", "method": method, "native": params, "tool": display, "input": input});
        if kind == DecisionKind::Elicitation {
            for (field, value) in [("serverName", &params["serverName"]), ("message", if params["message"].is_null() { &params["description"] } else { &params["message"] }),
                ("requestedSchema", &params["requestedSchema"]), ("url", &params["url"])] {
                if !value.is_null() {
                    payload[field] = value.clone();
                }
            }
        }
        let mut requested = json!({"type": "decision_requested", "request_id": key, "kind": kind, "payload": payload});
        if let Some(tool) = &tool {
            requested["tool_use_id"] = json!(tool);
        }
        let epoch = out.observe(requested);
        self.asked.insert(key, Asked { id, method, kind, epoch, payload, tool });
    }

    /// The daemon's answer reaches the decision the session waits on under exactly this identity.
    fn respond(&mut self, out: &mut Out, request_id: &str, epoch: u64, kind: DecisionKind, tool_use_id: Option<String>, decision: &Decision, asked: &Value) -> bool {
        let Some(entry) = self.asked.get(request_id).cloned() else { return false };
        if entry.epoch != epoch || entry.kind != kind || entry.tool != tool_use_id || entry.payload != *asked {
            return false;
        }
        let identity = sha256(entry.payload.to_string().as_bytes());
        if entry.method == "alondra/planApproval" {
            if self.answering_plans.contains(request_id) {
                return false;
            }
            let prompt = match decision {
                Decision::Superseded {} => {
                    self.answered.insert(request_id.to_owned(), identity);
                    self.settled(out, request_id, entry.epoch, "cancelled");
                    return true;
                }
                Decision::Allow { .. } => APPROVED_PLAN.to_owned(),
                Decision::Deny { message } => format!("Revise the plan using this user feedback: {message}"),
                _ => return false,
            };
            if self.queue.len() >= MAX_QUEUED {
                return false;
            }
            self.mode = if matches!(decision, Decision::Allow { .. }) { Mode::Default } else { Mode::Plan };
            self.answering_plans.insert(request_id.to_owned());
            self.answered.insert(request_id.to_owned(), identity);
            self.queue.push_front(Queued { id: format!("plan:{}", entry.epoch), prompt, plan: Some((request_id.to_owned(), entry)) });
            self.work.push_back(Work::Pump);
            return true;
        }
        let allow = matches!(decision, Decision::Allow { .. });
        let native = &entry.payload["native"];
        let result = match entry.method.as_str() {
            "item/tool/requestUserInput" => match decision {
                Decision::Allow { updated_input } => {
                    let answers = object(updated_input.get("answers").unwrap_or(&Value::Null));
                    let questions = native["questions"].as_array().cloned().unwrap_or_default();
                    if native["questions"].as_array().is_none() || questions.iter().any(|question| {
                        let id = question["id"].as_str().map_or_else(|| question["id"].to_string(), str::to_owned);
                        !answers.get(&id).is_some_and(|answer| answer["answers"].is_array())
                    }) {
                        return false;
                    }
                    json!({"answers": answers})
                }
                _ => json!({"answers": {}}),
            },
            "item/permissions/requestApproval" => json!({"permissions": if allow { Value::Object(object(&native["permissions"])) } else { json!({}) }, "scope": "turn", "strictAutoReview": true}),
            "mcpServer/elicitation/request" => match decision {
                Decision::Accept { content } => json!({"action": "accept", "content": content, "_meta": null}),
                _ => json!({"action": "decline", "content": null, "_meta": null}),
            },
            _ => {
                if allow && native["availableDecisions"].as_array().is_some_and(|available| !available.contains(&json!("accept"))) {
                    return false;
                }
                json!({"decision": if allow { "accept" } else { "decline" }})
            }
        };
        let identity = sha256(json!({"method": entry.method, "params": native}).to_string().as_bytes());
        self.answered.insert(request_id.to_owned(), identity);
        self.asked.remove(request_id);
        self.work.push_back(Work::Reply { id: entry.id.clone(), result, key: request_id.to_owned(), epoch: entry.epoch });
        true
    }

    fn settled(&mut self, out: &mut Out, key: &str, epoch: u64, outcome: &str) {
        self.asked.remove(key);
        out.observe(json!({"type": "decision_settled", "request_id": key, "epoch": epoch, "outcome": outcome}));
    }

    fn interrupt(&mut self, out: &mut Out) {
        if let (Some(thread), Some(turn)) = (self.thread.clone(), self.turn.clone()) {
            let _ = self.rpc(out, "turn/interrupt", json!({"threadId": thread, "turnId": turn}));
        }
    }

    fn force_stop(&mut self, out: &mut Out) {
        self.interrupt(out);
        let threads: Vec<String> = self.thread.iter().cloned().chain(self.children.iter().cloned()).collect();
        for thread in threads {
            if Some(&thread) != self.thread.as_ref()
                && let Ok(read) = self.rpc(out, "thread/turns/list", json!({"threadId": thread, "limit": 1, "sortDirection": "desc", "itemsView": "notLoaded"}))
                && let Some(running) = read["data"].as_array().and_then(|turns| turns.iter().find(|turn| turn["status"] == "inProgress")).and_then(|turn| text(&turn["id"]))
            {
                let _ = self.rpc(out, "turn/interrupt", json!({"threadId": thread, "turnId": running}));
            }
            if let Ok(page) = self.rpc(out, "thread/backgroundTerminals/list", json!({"threadId": thread, "cursor": null, "limit": 100})) {
                for terminal in page["data"].as_array().cloned().unwrap_or_default() {
                    let _ = self.rpc(out, "thread/backgroundTerminals/terminate", json!({"threadId": thread, "processId": terminal["processId"]}));
                }
            }
        }
        self.close(out);
    }

    fn supported_models(&mut self, out: &mut Out) -> Vec<String> {
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let Ok(page) = self.rpc(out, "model/list", json!({"cursor": cursor, "limit": 100, "includeHidden": false})) else { return models };
            models.extend(page["data"].as_array().cloned().unwrap_or_default().iter().filter_map(|entry| text(&entry["model"])));
            cursor = page["nextCursor"].clone();
            if !cursor.is_string() || models.len() > 1000 {
                return models;
            }
        }
    }
}
