//! A model of the Claude host: the Agent SDK host and the native Claude behind it, played in the
//! test's own process. It speaks the daemon's wire protocol with the real codec and proofs on the
//! connection it is given, and says what a Claude session says: the native messages the SDK
//! reports, framed as the host frames them. The world decides what the provider answers: whether
//! each account is signed in, and whether the API fails for everyone.
//!
//! How a turn goes is `Turns`: the test ends each one (`Played`), Claude answers each prompt at once
//! as the repository's fake native Claude does (`Answered`), or Claude never takes a prompt up
//! (`Ignored`).
//!
//! The model reaches its hosts through `Process`: the doubled hosts of `engine-doubles` (`Serve`),
//! or a process a test started for each host.

use alondra_agent_protocol::{
    Action, Attach, Attached, Body, Challenge, Command, Decision, DecisionKind, Fence, Frame, FramedReader, FramedWriter, Hello, PROTOCOL_VERSION, command_digest,
    host_proof, verify_owner_proof,
};
use alondra_engine_doubles::agent_host::{Serve, Served, Stop};
use alondra_engine_ports::agent_host::{ProbeOutput, ProbeRun};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// What the Claude host announces in its hello: every action the protocol carries.
const CAPABILITIES: &[&str] = &[
    "streaming_input", "owner_attach", "commit_ack", "exact_decisions", "interrupt", "force_stop", "set_model", "set_permission_mode",
    "mcp_status", "mcp_reconnect", "mcp_sign_in", "mcp_clear_auth", "plan_supersession",
];

/// The SDK's own list of what it reports, as the host's contract test keeps it.
const SDK_ERRORS: &str = include_str!("../../agent-protocol/fixtures/sdk-errors.json");

/// The models Claude offers to switch to, as the repository's fake native Claude lists them.
const OFFERED_MODELS: &[&str] = &["fixture-model"];

/// Claude's tool for Alondra's questions, served by the Alondra plugin.
const ALONDRA_TOOL: &str = "mcp__plugin_alondra_alondra__ask";
const ALONDRA_SERVER: &str = "plugin:alondra:alondra";
const FIXTURE_PLAN: &str = "# Fixture plan\n\n- [ ] Change the fixture\n- [ ] Test the fixture";
/// What the SDK answers Claude for a request no answer reached.
const CANCELLED: &str = "Permission request cancelled";

/// The exit of a process SIGKILL ended.
const KILLED: i32 = 9;

/// The errors the SDK ends an assistant message with when the API refused it.
pub fn assistant_errors() -> Vec<String> {
    let catalog: BTreeMap<String, Vec<String>> = serde_json::from_str(SDK_ERRORS).expect("the SDK's error catalog is JSON");
    catalog.get("SDKAssistantMessageError").cloned().unwrap_or_default()
}

/// The model every account offers.
pub const MODEL: &str = "demo-model";

/// How the API answers a turn that fails: the error the SDK names and the HTTP status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiError {
    pub error: String,
    pub status: Option<u16>,
    /// Claude Code's finer code of the error (`api_error`), such as `connection_failed`.
    pub detail: Option<String>,
    /// What Claude Code writes of the error in its synthetic message.
    pub said: Option<String>,
}

impl ApiError {
    /// The error an expired sign-in ends every turn with.
    pub fn signed_out() -> Self {
        Self { error: "authentication_failed".into(), status: Some(401), ..Self::default() }
    }
}

/// How Claude takes the prompts its host is offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Turns {
    /// A prompt opens a turn when none runs; the test ends each turn (`end_turn`) or has Claude read
    /// a message in it (`read`).
    #[default]
    Played,
    /// Claude answers each prompt at once with a result, as the fake native Claude does.
    Answered,
    /// Claude never takes a prompt up: each node holds its slot until its task ends.
    Ignored,
}

/// The provider's side of the world: its accounts and whether its API answers.
#[derive(Default)]
struct World {
    /// The accounts whose sign-in expired, by id.
    signed_out: BTreeSet<String>,
    /// The API fails every turn of these tasks with this error; of every task when none is named.
    outage: Option<(ApiError, Vec<String>)>,
    /// When the usage limit the API answers with resets, in milliseconds: an hour after the API
    /// began to answer with it.
    limit_resets_at: Option<i64>,
    /// Tasks no new host can start for.
    unstartable: BTreeSet<String>,
    /// How many tokens each session's conversation holds, for the tasks whose answers report what
    /// their requests cost; the hosts of other tasks report nothing.
    conversations: BTreeMap<String, u64>,
    sized: BTreeSet<String>,
    turns: Turns,
}

/// What went wrong between the daemon and the hosts, or something a host was asked it does not do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trouble(pub String);

/// A native Claude a host started: the session it runs, the one it resumed, how many prompts that
/// session had already, the folder and the command line and environment it got.
#[derive(Clone, Debug)]
pub struct NativeStart {
    pub session: String,
    pub resume: Option<String>,
    pub history: usize,
    pub cwd: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

impl NativeStart {
    /// The value the command line gives `flag`, as `--flag value` or `--flag=value`.
    pub fn flag(&self, flag: &str) -> Option<String> {
        flag_of(&self.arguments, flag)
    }

    /// The start as the fake native Claude records it.
    pub fn record(&self) -> Value {
        json!({"session": self.session, "resume": self.resume, "history": self.history, "cwd": self.cwd, "config": self.environment.get("CLAUDE_CONFIG_DIR")})
    }
}

/// The value `arguments` give `flag`, as `--flag value` or `--flag=value`.
pub fn flag_of(arguments: &[String], flag: &str) -> Option<String> {
    let inline = format!("{flag}=");
    arguments.iter().enumerate().find_map(|(at, argument)| {
        argument.strip_prefix(&inline).map(str::to_owned).or_else(|| (argument == flag).then(|| arguments.get(at + 1).cloned()).flatten())
    })
}

/// The model itself, shared by whatever starts its hosts and the test that plays the provider.
#[derive(Clone)]
pub struct ClaudeModel(Arc<Shared>);

struct Shared {
    home: PathBuf,
    world: Mutex<World>,
    hosts: Mutex<Vec<Arc<Host>>>,
    /// Every frame either side wrote: a test waits for it to stop moving.
    activity: AtomicU64,
    /// Hosts started that no daemon attached to yet.
    starting: Arc<AtomicU64>,
    trouble: Mutex<Option<Trouble>>,
    /// The runs that reached for something the model does not answer.
    leaks: Arc<Mutex<Vec<String>>>,
    next_uuid: AtomicU64,
    /// The clock and runtime every host reads the time on.
    clock: alondra_core::clock::Clock,
    runtime: Option<tokio::runtime::Handle>,
    startup_probe: (Mutex<bool>, std::sync::Condvar),
    /// No host starts any more: the test that played them is over.
    closed: AtomicBool,
    starts: Mutex<Vec<NativeStart>>,
    prompts: Mutex<Vec<String>>,
    /// The prompts each native session took, across the hosts that ran it.
    sessions: Mutex<BTreeMap<String, usize>>,
    /// What each native Claude got for each decision it asked for, as the fake native Claude
    /// records it.
    decisions: Mutex<Vec<Value>>,
    /// Every control request the native Claudes got, as the SDK sent it.
    controls: Mutex<Vec<Value>>,
    /// Beside this path Claude writes the plan it asks about, as the fake native Claude does
    /// beside its capture.
    capture: Mutex<Option<PathBuf>>,
}

pub struct StartupProbeGate(ClaudeModel);

impl Drop for StartupProbeGate {
    fn drop(&mut self) {
        *lock(&self.0.0.startup_probe.0) = false;
        self.0.0.startup_probe.1.notify_all();
    }
}

/// The process a host runs as: whether it still runs, and how it ends.
pub trait Process: Send + Sync {
    fn alive(&self) -> bool;
    /// The process ends: by itself once its work is closed, or killed with whatever it started.
    fn end(&self, closed: bool);
}

/// One host the daemon started: its process, its identity, and the conversation it holds.
pub struct Host {
    pub task: String,
    pub account: String,
    pub session: String,
    /// The node's capability, which the daemon gave its Claude in the environment.
    pub capability: String,
    /// The session this host resumes, when it continues one.
    pub resumes: bool,
    /// The number the host's process goes by.
    pub process_id: u32,
    /// No daemon attached to this host yet, counted in the model's hosts starting.
    unattached: AtomicBool,
    starting: Arc<AtomicU64>,
    process: Box<dyn Process>,
    bootstrap: Bootstrap,
    secret: [u8; 32],
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    lane: Option<FramedWriter<UnixStream>>,
    /// The host's end of every connection, closed when the host ends.
    connections: Vec<UnixStream>,
    /// Every event this host sent, by cursor, until the daemon acknowledges it.
    journal: VecDeque<(u64, Frame)>,
    cursor: u64,
    acknowledged: u64,
    receipts: BTreeMap<String, Frame>,
    /// The inputs the running turn took, in order.
    turn: Option<Vec<String>>,
    /// Inputs offered while a turn ran, by native id with their prompt, which Claude reads in the
    /// turn or takes as the next one.
    queued: VecDeque<(String, String)>,
    /// The prompt of every input the host was offered, by native id.
    prompts: BTreeMap<String, String>,
    /// The host closed: it ends once the daemon acknowledged its last event.
    closing: bool,
    /// Claude's session runs nothing.
    busy: bool,
    ended: bool,
    /// What the first request of this host's session wrote to the prompt cache, once it answered.
    first_written: Option<u64>,
    /// What the daemon answered each permission Claude asked, by request: `allow` or `deny`.
    answers: BTreeMap<String, String>,
    /// The decisions Claude waits on, by request.
    asked: BTreeMap<String, Asked>,
    /// How many decisions this host's Claude asked for: each names its request and tool use.
    decisions: u64,
}

/// A decision Claude asked for and waits on: what the host announced, under the cursor that
/// announced it, and the inputs of the turn it holds.
struct Asked {
    kind: DecisionKind,
    tool: Option<String>,
    tool_use_id: Option<String>,
    payload: serde_json::Map<String, Value>,
    epoch: u64,
    taken: Vec<String>,
    /// Claude ends the turn once it has the answer, as the fake native Claude does; a turn the test
    /// plays goes on until the test ends it.
    concludes: bool,
}

#[derive(Deserialize)]
struct Bootstrap {
    host_instance_id: String,
    control_secret: String,
    fence: Fence,
    execution: alondra_agent_protocol::Execution,
    manifest: Manifest,
    native: Native,
    #[serde(default)]
    worker: Option<Worker>,
    #[serde(default)]
    journal_base: u64,
}

#[derive(Deserialize)]
struct Manifest {
    host_release: String,
    host_sha256: String,
    executable_sha256: String,
    sdk_version: String,
}

#[derive(Deserialize)]
struct Native {
    session_id: String,
    #[serde(default)]
    cwd: PathBuf,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    resume: Option<Value>,
}

#[derive(Default, Deserialize)]
struct Worker {
    #[serde(default)]
    plugins: Vec<PathBuf>,
    #[serde(default)]
    settings: serde_json::Map<String, Value>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    disallowed_tools: Vec<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn decode_secret(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut secret = [0u8; 32];
    for (index, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(secret)
}

/// Which account a run of Claude is for: the account folder its configuration names, or the default
/// account's own when it names none.
fn account_of(home: &Path, environment: &BTreeMap<String, String>) -> String {
    let folders = alondra_core::accounts::folders(home);
    environment
        .get("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|folder| folder.parent() == Some(folders.as_path()))
        .and_then(|folder| folder.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| alondra_core::accounts::AccountId::default_account().as_str().to_owned())
}

impl Bootstrap {
    fn resumes(&self) -> bool {
        self.native.resume.as_ref().is_some_and(|resume| !resume.is_null() && *resume != Value::Bool(false))
    }

    /// The command line the SDK starts the native Claude of a task with, for what a test reads of it.
    fn arguments(&self) -> Vec<String> {
        let session = self.native.session_id.to_lowercase();
        let mut arguments = vec!["--output-format".to_owned(), "stream-json".into(), "--input-format".into(), "stream-json".into()];
        let worker = self.worker.as_ref();
        if let Some(model) = worker.and_then(|worker| worker.model.as_ref()) {
            arguments.extend(["--model".to_owned(), model.clone()]);
        }
        if let Some(effort) = worker.and_then(|worker| worker.effort.as_ref()) {
            arguments.extend(["--effort".to_owned(), effort.clone()]);
        }
        if let Some(tools) = worker.map(|worker| &worker.disallowed_tools).filter(|tools| !tools.is_empty()) {
            arguments.extend(["--disallowedTools".to_owned(), tools.join(",")]);
        }
        if let Some(worker) = worker {
            if !worker.settings.is_empty() {
                arguments.extend(["--settings".to_owned(), Value::Object(worker.settings.clone()).to_string()]);
            }
            for plugin in &worker.plugins {
                arguments.extend(["--plugin-dir".to_owned(), plugin.display().to_string()]);
            }
        }
        arguments.extend(["--setting-sources".to_owned(), "user,project,local".into(), "--permission-mode".into(), "auto".into()]);
        arguments.push(if self.resumes() { format!("--resume={session}") } else { format!("--session-id={session}") });
        arguments
    }
}

impl ClaudeModel {
    pub fn new(home: &Path, leaks: Arc<Mutex<Vec<String>>>) -> Self {
        Self(Arc::new(Shared {
            home: home.to_path_buf(),
            world: Mutex::default(),
            hosts: Mutex::default(),
            activity: AtomicU64::new(0),
            starting: Arc::new(AtomicU64::new(0)),
            trouble: Mutex::default(),
            leaks,
            next_uuid: AtomicU64::new(0),
            clock: alondra_core::clock::Clock::current(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            startup_probe: (Mutex::new(false), std::sync::Condvar::new()),
            closed: AtomicBool::new(false),
            starts: Mutex::default(),
            prompts: Mutex::default(),
            sessions: Mutex::default(),
            decisions: Mutex::default(),
            controls: Mutex::default(),
            capture: Mutex::default(),
        }))
    }

    /// Claude writes each plan it asks about beside `capture`.
    pub fn set_capture(&self, capture: &Path) {
        *lock(&self.0.capture) = Some(capture.to_path_buf());
    }

    /// What each native Claude got for each decision it asked for, in order: the request, whether
    /// Claude had withdrawn it, and the SDK's response.
    pub fn decisions(&self) -> Vec<Value> {
        lock(&self.0.decisions).clone()
    }

    /// Every control request the native Claudes got, in order.
    pub fn controls(&self) -> Vec<Value> {
        lock(&self.0.controls).clone()
    }

    /// How Claude takes the prompts of every host from now on.
    pub fn set_turns(&self, turns: Turns) {
        lock(&self.0.world).turns = turns;
    }

    pub fn hold_startup_probe(&self) -> StartupProbeGate {
        *lock(&self.0.startup_probe.0) = true;
        StartupProbeGate(self.clone())
    }

    pub fn activity(&self) -> u64 {
        self.0.activity.load(Ordering::SeqCst)
    }

    pub fn trouble(&self) -> Option<Trouble> {
        lock(&self.0.trouble).clone()
    }

    pub fn set_outage(&self, outage: Option<(ApiError, Vec<String>)>) {
        let mut world = lock(&self.0.world);
        let limited = outage.as_ref().is_some_and(|(error, _)| error.error == "rate_limit");
        world.limit_resets_at = limited.then(|| world.limit_resets_at.unwrap_or_else(|| alondra_core::now_millis() + 3_600_000));
        world.outage = outage;
    }

    pub fn set_signed_in(&self, account: &str, signed_in: bool) {
        let mut world = lock(&self.0.world);
        if signed_in {
            world.signed_out.remove(account);
        } else {
            world.signed_out.insert(account.to_owned());
        }
    }

    pub fn set_startable(&self, task: &str, startable: bool) {
        let mut world = lock(&self.0.world);
        if startable {
            world.unstartable.remove(task);
        } else {
            world.unstartable.insert(task.to_owned());
        }
    }

    /// The conversation of `task`'s node holds `tokens` from now on: the answers of its hosts say
    /// what each request read from the prompt cache and wrote to it, a resumed session's first
    /// request writing its whole conversation again, as one whose cache expired does, and Claude
    /// Code keeps each session's transcript where it keeps a real one.
    pub fn set_conversation(&self, task: &str, tokens: u64) {
        let running = self.running(task);
        let mut world = lock(&self.0.world);
        world.sized.insert(task.to_owned());
        if let Some(host) = running {
            world.conversations.insert(host.session.clone(), tokens);
        }
    }

    /// What the first request of the last host started for `task` wrote to the prompt cache.
    pub fn first_written(&self, task: &str) -> Option<u64> {
        self.hosts_of(task).last().and_then(|host| lock(&host.state).first_written)
    }

    /// The usage an answer of `host` reports, its request having sent `sent` new tokens; none for
    /// a task whose conversation is not sized.
    fn usage(&self, host: &Host, state: &mut HostState, sent: u64) -> Option<Value> {
        let mut world = lock(&self.0.world);
        if !world.sized.contains(&host.task) {
            return None;
        }
        let held = world.conversations.get(&host.session).copied().unwrap_or(0);
        let (written, read) = if state.first_written.is_none() && (host.resumes || held == 0) { (held + sent, 0) } else { (sent, held) };
        state.first_written.get_or_insert(written);
        world.conversations.insert(host.session.clone(), held + sent);
        Some(json!({"input_tokens": 3, "output_tokens": 100, "cache_creation_input_tokens": written, "cache_read_input_tokens": read,
            "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": written}}))
    }

    /// Appends the prompt to the native transcript of `host`'s session, for a sized task, as Claude
    /// Code writes it in its profile's project folder.
    fn transcribe_sized(&self, host: &Host, prompt: &str) {
        let cwd = host.bootstrap.native.cwd.to_string_lossy();
        if cwd.is_empty() || !lock(&self.0.world).sized.contains(&host.task) {
            return;
        }
        let environment = &host.bootstrap.native.env;
        let Some(home) = environment.get("HOME") else { return };
        let profile = alondra_core::claude_account::configured_profile(&|key: &str| environment.get(key).cloned());
        let bucket: String = cwd.chars().map(|character| if character.is_ascii_alphanumeric() { character } else { '-' }).collect();
        let folder = alondra_core::claude_account::config_folder(profile.as_deref(), Path::new(home)).join("projects").join(bucket);
        let record = json!({"type": "user", "sessionId": host.session, "cwd": cwd, "timestamp": "2026-11-19T10:00:00.000Z", "message": {"role": "user", "content": prompt}});
        let written = std::fs::create_dir_all(&folder).and_then(|()| {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(folder.join(format!("{}.jsonl", host.session)))?;
            writeln!(file, "{record}")
        });
        if let Err(error) = written {
            self.troubled(format!("the transcript of {} could not be written: {error}", host.session));
        }
    }

    /// Whether no new host can start for `task`.
    pub fn unstartable(&self, task: &str) -> bool {
        lock(&self.0.world).unstartable.contains(task)
    }

    /// Every native Claude the hosts started, in order.
    pub fn starts(&self) -> Vec<NativeStart> {
        lock(&self.0.starts).clone()
    }

    /// Every prompt Claude took, in order.
    pub fn prompts(&self) -> Vec<String> {
        lock(&self.0.prompts).clone()
    }

    /// Every host started for `task`, in the order they started.
    pub fn hosts_of(&self, task: &str) -> Vec<Arc<Host>> {
        lock(&self.0.hosts).iter().filter(|host| host.task == task).cloned().collect()
    }

    /// Each task whose host runs, with how many inputs that host was offered and has not taken
    /// yet: none while it rests between turns.
    pub fn untaken_offers(&self) -> Vec<(String, usize)> {
        lock(&self.0.hosts)
            .iter()
            .filter(|host| host.alive())
            .map(|host| {
                let state = lock(&host.state);
                (host.task.clone(), if state.turn.is_some() { state.queued.len() } else { 0 })
            })
            .collect()
    }

    /// The host of `task` that runs now.
    pub fn running(&self, task: &str) -> Option<Arc<Host>> {
        self.hosts_of(task).into_iter().rev().find(|host| host.alive())
    }

    /// Whether the daemon is attached to every host that runs and has acknowledged what each said.
    pub fn committed(&self) -> bool {
        self.0.starting.load(Ordering::SeqCst) == 0 && lock(&self.0.hosts).iter().all(|host| {
            let state = lock(&host.state);
            state.ended || !host.process.alive() || state.lane.is_some() && state.acknowledged >= state.cursor
        })
    }

    /// The process of `task`'s host dies, with whatever it started.
    pub fn kill(&self, task: &str) {
        if let Some(host) = self.running(task) {
            host.end(false);
        }
    }

    /// The host whose process goes by `process_id` dies, with whatever it started.
    pub fn kill_process(&self, process_id: u32) {
        if let Some(host) = self.host(process_id) {
            host.end(false);
        }
    }

    /// Ends every host still running: nothing a test started outlives it.
    pub fn end_all(&self) {
        let hosts = lock(&self.0.hosts).clone();
        for host in hosts {
            host.end(false);
        }
    }

    /// Ends every host and starts no other: once it returns, no host of the model writes anything.
    pub fn close(&self) {
        {
            let _hosts = lock(&self.0.hosts);
            self.0.closed.store(true, Ordering::SeqCst);
        }
        self.end_all();
    }

    /// A host starts with `bootstrap`, as the process `process` numbered `process_id`; `serve`
    /// serves each of its connections.
    pub fn start(&self, bootstrap: &[u8], process_id: u32, process: Box<dyn Process>) -> io::Result<Arc<Host>> {
        let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
        let bootstrap: Bootstrap = serde_json::from_slice(bootstrap).map_err(|e| invalid(e.to_string()))?;
        let secret = decode_secret(&bootstrap.control_secret).ok_or_else(|| invalid("the control secret is not 32 bytes of hex".into()))?;
        let alondra_agent_protocol::Execution::Task(execution) = &bootstrap.execution else {
            return Err(invalid("the model plays no orchestrator host".into()));
        };
        let task = execution.task_id.to_string();
        let session = bootstrap.native.session_id.to_lowercase();
        let resumes = bootstrap.resumes();
        let history = lock(&self.0.sessions).get(&session).copied().unwrap_or_default();
        lock(&self.0.starts).push(NativeStart {
            session: session.clone(),
            resume: resumes.then(|| session.clone()),
            history,
            cwd: bootstrap.native.cwd.clone(),
            arguments: bootstrap.arguments(),
            environment: bootstrap.native.env.clone(),
        });
        self.0.starting.fetch_add(1, Ordering::SeqCst);
        let host = Arc::new(Host {
            account: account_of(&self.0.home, &bootstrap.native.env),
            capability: bootstrap.native.env.get("ALONDRA_DOMAIN_CAPABILITY").cloned().unwrap_or_default(),
            session: bootstrap.native.session_id.clone(),
            resumes,
            process_id,
            unattached: AtomicBool::new(true),
            starting: self.0.starting.clone(),
            process,
            task,
            secret,
            state: Mutex::new(HostState { cursor: bootstrap.journal_base, acknowledged: bootstrap.journal_base, ..HostState::default() }),
            bootstrap,
        });
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

    /// Serves one daemon's connection to `host`: the attach, what it had not acknowledged, then
    /// every frame it sends, until it goes. It returns once the connection closes.
    pub fn serve(&self, host: &Arc<Host>, stream: UnixStream) {
        serve(self, host, stream);
    }

    /// Runs `work` on a thread of its own, on the model's clock.
    pub fn spawn(&self, work: impl FnOnce() + Send + 'static) {
        let (clock, runtime) = (self.0.clock, self.0.runtime.clone());
        std::thread::spawn(move || {
            let _entered = alondra_core::clock::enter(clock, runtime.as_ref());
            work();
        });
    }

    /// The host whose process goes by `process_id`.
    fn host(&self, process_id: u32) -> Option<Arc<Host>> {
        lock(&self.0.hosts).iter().find(|host| host.process_id == process_id).cloned()
    }

    /// The error the API answers a turn of `host` with now, if it fails.
    fn failure_for(&self, host: &Host) -> Option<ApiError> {
        let world = lock(&self.0.world);
        if world.signed_out.contains(&host.account) {
            return Some(ApiError::signed_out());
        }
        let (error, tasks) = world.outage.as_ref()?;
        (tasks.is_empty() || tasks.contains(&host.task)).then(|| error.clone())
    }

    /// When the usage limit resets: the world's, or an hour from now for a turn that alone hit it.
    fn limit_resets_at(&self) -> i64 {
        lock(&self.0.world).limit_resets_at.unwrap_or_else(|| alondra_core::now_millis() + 3_600_000)
    }

    fn uuid(&self) -> String {
        format!("00000000-0000-4000-9000-{:012x}", self.0.next_uuid.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn tick(&self) {
        self.0.activity.fetch_add(1, Ordering::SeqCst);
    }

    fn troubled(&self, why: String) {
        lock(&self.0.trouble).get_or_insert(Trouble(why));
    }

    /// The turn of `task`'s node ends: on the API's error while it fails, or with an answer.
    pub fn end_turn(&self, task: &str, error: Option<ApiError>) -> Result<(), String> {
        let host = self.running(task).ok_or_else(|| format!("no host of {task} runs"))?;
        let error = error.or_else(|| self.failure_for(&host));
        let mut state = lock(&host.state);
        if state.turn.is_none() {
            return Err(format!("the node of {task} has no turn running"));
        }
        self.finish_turn(&host, &mut state, error);
        self.next_turn(&host, &mut state);
        Ok(())
    }

    /// Claude Code loses the response it was reading in `task`'s turn, as when the Mac sleeps
    /// mid-response: it writes the synthetic API error message it writes then, and goes on with the
    /// same turn, which a later `end_turn` ends.
    pub fn lose_response(&self, task: &str) -> Result<(), String> {
        let host = self.running(task).ok_or_else(|| format!("no host of {task} runs"))?;
        let mut state = lock(&host.state);
        if state.turn.is_none() {
            return Err(format!("the node of {task} has no turn running"));
        }
        let uuid = self.uuid();
        let message = json!({"type": "assistant", "uuid": uuid, "parent_tool_use_id": null, "error": "server_error", "api_error": "computer_slept",
            "is_api_error_message": true, "session_id": host.session, "message": {"role": "assistant", "model": "<synthetic>",
            "content": [{"type": "text", "text": "API Error: Your computer went to sleep mid-response. The response above may be incomplete."}]}});
        self.emit(&host, &mut state, "message", message);
        Ok(())
    }

    /// Claude reads the message the user wrote with `text` while its turn ran, in that same turn.
    pub fn read(&self, task: &str, text: &str) -> Result<(), String> {
        let host = self.running(task).ok_or_else(|| format!("no host of {task} runs"))?;
        let mut state = lock(&host.state);
        let Some(mut taken) = state.turn.clone() else { return Err(format!("the node of {task} has no turn running")) };
        if taken.iter().any(|input| state.prompts.get(input).is_some_and(|prompt| prompt.contains(text))) {
            return Ok(());
        }
        let Some(position) = state.queued.iter().position(|(_, prompt)| prompt.contains(text)) else {
            let offered: Vec<&String> = state.prompts.values().collect();
            return Err(format!("the host of {task} was never offered a message with {text:?}; it was offered {offered:?}"));
        };
        let (input, _) = state.queued.remove(position).unwrap_or_default();
        taken.push(input.clone());
        let uuid = self.uuid();
        let message = json!({"type": "assistant", "uuid": uuid, "parent_tool_use_id": null, "user_message_uuid": input, "user_message_uuids": taken,
            "message": {"role": "assistant", "content": [{"type": "text", "text": "Read"}]}, "session_id": host.session});
        self.emit(&host, &mut state, "message", message);
        state.turn = Some(taken);
        Ok(())
    }

    /// Claude asks permission to run `command` in Bash from `cwd`, in the turn it runs, as Claude
    /// Code asks for a tool its rules do not allow; `request` names the request.
    pub fn ask_permission(&self, task: &str, request: &str, command: &str, cwd: &str) -> Result<(), String> {
        let host = self.running(task).ok_or_else(|| format!("no host of {task} runs"))?;
        let mut state = lock(&host.state);
        if state.turn.is_none() {
            return Err(format!("the node of {task} has no turn running to ask from"));
        }
        let tool_use = format!("toolu_{}", self.uuid().replace('-', ""));
        let payload = json!({"tool": "Bash", "input": {"command": command}, "request": {}, "cwd": cwd});
        self.event(&host, &mut state, json!({"type": "decision_requested", "request_id": request, "tool_use_id": tool_use, "kind": "permission", "payload": payload}));
        let Value::Object(payload) = payload else { return Ok(()) };
        let asked = Asked { kind: DecisionKind::Permission, tool: Some("Bash".into()), tool_use_id: Some(tool_use), payload, epoch: state.cursor,
            taken: state.turn.clone().unwrap_or_default(), concludes: false };
        state.asked.insert(request.to_owned(), asked);
        Ok(())
    }

    /// What the daemon answered the permission `request` of `task`'s node, if it has.
    pub fn answer_of(&self, task: &str, request: &str) -> Option<String> {
        self.hosts_of(task).iter().find_map(|host| lock(&host.state).answers.get(request).cloned())
    }

    /// An input reaches the host, and Claude takes it as `Turns` says.
    fn offered(&self, host: &Host, state: &mut HostState, input: String, prompt: String) {
        let turns = lock(&self.0.world).turns;
        if turns == Turns::Ignored {
            return;
        }
        lock(&self.0.prompts).push(prompt.clone());
        *lock(&self.0.sessions).entry(host.session.to_lowercase()).or_default() += 1;
        match turns {
            Turns::Played => self.queue(host, state, input, prompt),
            Turns::Answered => self.answer(host, state, input, &prompt),
            Turns::Ignored => {}
        }
    }

    /// The input opens a turn when none runs, and waits in Claude's queue when one does. A turn the
    /// API refuses ends at once.
    fn queue(&self, host: &Host, state: &mut HostState, input: String, prompt: String) {
        let queued = self.uuid();
        self.emit(host, state, "status", json!({"type": "command_lifecycle", "state": "queued", "uuid": queued, "command_uuid": input, "session_id": host.session}));
        self.transcribe_sized(host, &prompt);
        state.prompts.insert(input.clone(), prompt.clone());
        state.queued.push_back((input, prompt));
        if state.turn.is_none() {
            self.next_turn(host, state);
        }
    }

    /// Claude queues and starts the input, announcing its session when no turn holds it, and ends
    /// the turn with a result for every input it took, as the fake native Claude does with a prompt
    /// it has no script for. `consume-and-hold` is taken up and the turn kept open, as the fake
    /// does. The prompt goes into the session's transcript in Claude's profile, which the daemon
    /// reads before it resumes the session.
    fn answer(&self, host: &Host, state: &mut HostState, input: String, prompt: &str) {
        let session = host.session.to_lowercase();
        self.transcribe(host, &input, prompt);
        let mut taken = state.turn.take().unwrap_or_default();
        taken.push(input.clone());
        for lifecycle in ["queued", "started"] {
            let uuid = self.uuid();
            self.emit(host, state, "status", json!({"type": "command_lifecycle", "command_uuid": input, "state": lifecycle, "session_id": session, "uuid": uuid}));
        }
        if taken.len() == 1 {
            let (title, init) = (self.uuid(), self.uuid());
            self.emit(host, state, "status", json!({"type": "system", "subtype": "session_title_changed", "title": "fixture", "session_id": session, "uuid": title}));
            self.emit(host, state, "initialized", json!({"type": "system", "subtype": "init", "session_id": session, "uuid": init, "claude_code_version": "2.1.300-fixture",
                "model": "fixture", "cwd": host.bootstrap.native.cwd, "tools": ["Bash"], "mcp_servers": [], "permissionMode": "default", "slash_commands": [],
                "output_style": "default", "skills": [], "plugins": [], "apiKeySource": "none"}));
        }
        if taken.len() == 1 && let Some(said) = decision_scenario(prompt) {
            return self.decide(host, state, taken, said);
        }
        if prompt == "consume-and-hold" {
            let consumed = self.uuid();
            self.emit(host, state, "message", json!({"type": "assistant", "session_id": session, "uuid": consumed, "parent_tool_use_id": null,
                "message": {"id": format!("msg_{consumed}"), "type": "message", "role": "assistant", "model": "fixture", "content": [{"type": "text", "text": "Fixture consumed input"}],
                    "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}},
                "user_message_uuid": input, "user_message_uuids": taken}));
            state.turn = Some(taken);
            return;
        }
        if taken.len() > 1 {
            let notification = self.uuid();
            self.emit(host, state, "task", json!({"type": "system", "subtype": "task_notification", "task_id": "fixture-background", "status": "completed",
                "output_file": "fixture-output", "summary": "Fixture work completed", "uuid": notification, "session_id": session}));
        }
        self.conclude(host, state, &input, &taken);
    }

    /// The turn that took `taken` ends with a result for `input`, and every input it took completes.
    fn conclude(&self, host: &Host, state: &mut HostState, input: &str, taken: &[String]) {
        let session = host.session.to_lowercase();
        let result = self.uuid();
        self.emit(host, state, "result", json!({"type": "result", "subtype": "success", "is_error": false, "session_id": session, "uuid": result,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "user_message_uuid": input, "result": "Fixture result", "terminal_reason": "completed", "user_message_uuids": taken}));
        for input in taken {
            let completed = self.uuid();
            self.emit(host, state, "status", json!({"type": "command_lifecycle", "command_uuid": input, "state": "completed", "session_id": session, "uuid": completed}));
        }
    }

    /// Claude calls a tool its rules do not allow, or an MCP server asks for input, as the fake
    /// native Claude does for `decide:<scenario> <argument>`; the host asks the daemon and the
    /// turn waits for the answer.
    fn decide(&self, host: &Host, state: &mut HostState, taken: Vec<String>, said: &str) {
        let (scenario, argument) = said.split_once(' ').unwrap_or((said, ""));
        let (scenario, cwd) = match scenario.strip_prefix("bash@") {
            Some(folder) => ("bash", folder.to_owned()),
            None => (scenario, std::fs::canonicalize(&host.bootstrap.native.cwd).unwrap_or_else(|_| host.bootstrap.native.cwd.clone()).display().to_string()),
        };
        state.decisions += 1;
        let (request_id, tool_use_id) = (format!("decision-{}", state.decisions), format!("toolu_fixture_{}", state.decisions));
        let question = json!({"question": "Which fixture color?", "header": "Color", "multiSelect": false,
            "options": [{"label": "Red", "description": "The red fixture"}, {"label": "Blue", "description": "The blue fixture"}]});
        let sizes = json!({"question": "Which fixture sizes?", "header": "Sizes", "multiSelect": true,
            "options": [{"label": "Small", "description": "The small fixture"}, {"label": "Large", "description": "The large fixture"}]});
        let mut request = serde_json::Map::new();
        let mut called = None;
        let (kind, tool, input) = match scenario {
            "bash" => (DecisionKind::Permission, "Bash", json!({"command": if argument.is_empty() { "echo fixture" } else { argument }})),
            "question" => (DecisionKind::Question, "AskUserQuestion", json!({"questions": [question]})),
            "questions" => (DecisionKind::Question, "AskUserQuestion", json!({"questions": [question, sizes]})),
            "plan" | "unwritten-plan" => {
                called = Some(json!({}));
                if scenario == "unwritten-plan" {
                    (DecisionKind::Plan, "ExitPlanMode", json!({}))
                } else {
                    let plan = if argument.is_empty() { FIXTURE_PLAN } else { argument };
                    let file = format!("{}.plan.md", self.capture().display());
                    if let Err(error) = std::fs::write(&file, plan) {
                        self.troubled(format!("the plan of {}: {error}", host.task));
                    }
                    (DecisionKind::Plan, "ExitPlanMode", json!({"plan": plan, "planFilePath": file}))
                }
            }
            "alondra" | "imitation" => {
                let source = if scenario == "alondra" { "plugin" } else { "user" };
                request.insert("mcpServer".into(), json!({"name": ALONDRA_SERVER, "source": source}));
                (DecisionKind::Permission, ALONDRA_TOOL, json!({"need": "Fixture need"}))
            }
            "elicitation" => (DecisionKind::Elicitation, "", Value::Null),
            other => return self.troubled(format!("the model asks no decision {other:?}")),
        };
        let input_id = taken.last().cloned().unwrap_or_default();
        let session = host.session.to_lowercase();
        let tool = (kind != DecisionKind::Elicitation).then(|| tool.to_owned());
        if let Some(tool) = &tool {
            let consumed = self.uuid();
            let block = json!({"type": "tool_use", "id": tool_use_id, "name": tool, "input": called.unwrap_or_else(|| input.clone())});
            self.emit(host, state, "message", json!({"type": "assistant", "session_id": session, "uuid": consumed, "parent_tool_use_id": null,
                "message": {"id": format!("msg_{consumed}"), "type": "message", "role": "assistant", "model": "fixture", "content": [block],
                    "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}},
                "user_message_uuid": input_id, "user_message_uuids": taken}));
            request.insert("toolUseID".into(), json!(tool_use_id));
        }
        request.insert("requestId".into(), json!(request_id));
        let payload = match &tool {
            Some(tool) => json!({"tool": tool, "input": input, "request": request, "cwd": cwd}),
            None => json!({"serverName": "fixture-server", "message": "Which fixture name?", "mode": "form",
                "requestedSchema": {"type": "object", "properties": {"name": {"type": "string", "title": "Name"}}, "required": ["name"]}}),
        };
        let mut observation = json!({"type": "decision_requested", "request_id": request_id, "kind": kind, "payload": payload});
        let tool_use_id = tool.is_some().then_some(tool_use_id);
        if let Some(id) = &tool_use_id {
            observation["tool_use_id"] = json!(id);
        }
        self.event(host, state, observation);
        let Value::Object(payload) = payload else { return };
        let asked = Asked { kind, tool, tool_use_id, payload, epoch: state.cursor, taken: taken.clone(), concludes: true };
        state.asked.insert(request_id, asked);
        state.turn = Some(taken);
    }

    /// Where Claude writes what the fake native Claude writes beside its capture.
    fn capture(&self) -> PathBuf {
        lock(&self.0.capture).clone().unwrap_or_else(|| self.0.home.join("fake-claude-capture"))
    }

    /// The daemon's answer reaches the request Claude waits on under exactly this identity, as the
    /// host applies it; the SDK hands Claude its response and Claude goes on with its turn.
    fn respond(&self, host: &Host, state: &mut HostState, respond: Responded) -> Result<(), &'static str> {
        let Responded { request_id, epoch, kind, tool_use_id, decision, asked } = respond;
        let waiting = state.asked.get(&request_id).ok_or("Claude is not waiting for this request")?;
        let tool_decision = matches!(decision, Decision::Allow { .. } | Decision::Deny { .. });
        if waiting.epoch != epoch || waiting.kind != kind || waiting.tool_use_id != tool_use_id || waiting.payload != asked || (kind != DecisionKind::Elicitation) != tool_decision {
            return Err("Claude is not waiting for this request");
        }
        let waiting = state.asked.remove(&request_id).ok_or("Claude is not waiting for this request")?;
        state.answers.insert(request_id.clone(), match decision { Decision::Allow { .. } => "allow", Decision::Deny { .. } => "deny", _ => "other" }.to_owned());
        let response = sdk_response(&waiting, Some(decision));
        self.decided(&waiting, &request_id, false, response.clone());
        self.event(host, state, json!({"type": "decision_settled", "request_id": request_id, "epoch": epoch, "outcome": "answered"}));
        if !waiting.concludes {
            return Ok(());
        }
        if let Some(tool_use_id) = &waiting.tool_use_id {
            let allowed = response["behavior"] == "allow";
            let content = if allowed { response["updatedInput"].to_string() } else { response["message"].as_str().unwrap_or("denied").to_owned() };
            let uuid = self.uuid();
            self.emit(host, state, "message", json!({"type": "user", "session_id": host.session.to_lowercase(), "uuid": uuid, "parent_tool_use_id": null,
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_use_id, "is_error": !allowed, "content": content}]}}));
        }
        if state.turn.as_ref() == Some(&waiting.taken) && state.asked.is_empty() {
            state.turn = None;
            let input = waiting.taken.last().cloned().unwrap_or_default();
            self.conclude(host, state, &input, &waiting.taken);
        }
        Ok(())
    }

    /// The native Claude got `response` for `request_id`, which it had withdrawn when `cancelled`.
    fn decided(&self, asked: &Asked, request_id: &str, cancelled: bool, response: Value) {
        let mut record = json!({"request_id": request_id, "subtype": if asked.tool.is_some() { "can_use_tool" } else { "elicitation" },
            "cancelled": cancelled, "outcome": "success", "response": response});
        if let (Some(tool), Some(tool_use_id)) = (&asked.tool, &asked.tool_use_id) {
            record["tool_name"] = json!(tool);
            record["tool_use_id"] = json!(tool_use_id);
        }
        lock(&self.0.decisions).push(record);
    }

    /// Claude withdraws every decision it waits on: the SDK answers each as cancelled and the host
    /// settles it so.
    fn withdraw(&self, host: &Host, state: &mut HostState, answered: bool) {
        for (request_id, asked) in std::mem::take(&mut state.asked) {
            if answered {
                self.decided(&asked, &request_id, true, sdk_response(&asked, None));
            }
            self.event(host, state, json!({"type": "decision_settled", "request_id": request_id, "epoch": asked.epoch, "outcome": "cancelled"}));
        }
    }

    /// Appends the prompt to the session's transcript where the native Claude keeps it: under the
    /// profile its environment names, in the folder named after its working directory.
    fn transcribe(&self, host: &Host, input: &str, prompt: &str) {
        let environment = &host.bootstrap.native.env;
        let profile = ["CLAUDE_CONFIG_DIR", "CLAUDE_HOME"].iter().find_map(|key| environment.get(*key).filter(|value| !value.trim().is_empty()).map(PathBuf::from))
            .or_else(|| environment.get("HOME").map(|home| Path::new(home).join(".claude")));
        let Some(profile) = profile else { return };
        let cwd = std::fs::canonicalize(&host.bootstrap.native.cwd).unwrap_or_else(|_| host.bootstrap.native.cwd.clone());
        let project: String = cwd.to_string_lossy().chars().map(|character| if character.is_ascii_alphanumeric() { character } else { '-' }).collect();
        let folder = profile.join("projects").join(project);
        let line = json!({"type": "user", "sessionId": host.session.to_lowercase(), "cwd": cwd, "uuid": input, "message": {"role": "user", "content": prompt}});
        let written = std::fs::create_dir_all(&folder).and_then(|()| {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(folder.join(format!("{}.jsonl", host.session.to_lowercase())))?;
            writeln!(file, "{line}")
        });
        if let Err(error) = written {
            self.troubled(format!("the transcript of {}: {error}", host.task));
        }
    }

    /// Claude takes the next input waiting as a turn, and the next while the API refuses each.
    /// It answers each turn it starts at once; the turn ends when the test says.
    fn next_turn(&self, host: &Host, state: &mut HostState) {
        while state.turn.is_none() {
            let Some((input, _)) = state.queued.pop_front() else {
                if std::mem::replace(&mut state.busy, false) {
                    let idle = self.uuid();
                    self.emit(host, state, "status", json!({"type": "system", "subtype": "session_state_changed", "state": "idle", "uuid": idle, "session_id": host.session}));
                }
                return;
            };
            if !std::mem::replace(&mut state.busy, true) {
                let running = self.uuid();
                self.emit(host, state, "status", json!({"type": "system", "subtype": "session_state_changed", "state": "running", "uuid": running, "session_id": host.session}));
            }
            let (started, init) = (self.uuid(), self.uuid());
            self.emit(host, state, "status", json!({"type": "command_lifecycle", "state": "started", "uuid": started, "command_uuid": input, "session_id": host.session}));
            self.emit(host, state, "initialized", json!({"type": "system", "subtype": "init", "uuid": init, "user_message_uuids": [input], "session_id": host.session}));
            state.turn = Some(vec![input.clone()]);
            match self.failure_for(host) {
                Some(error) => self.finish_turn(host, state, Some(error)),
                None => {
                    let answer = self.uuid();
                    let sent = state.prompts.get(&input).map_or(0, |prompt| prompt.len() as u64 / 4) + 1_000;
                    let mut message = json!({"type": "assistant", "uuid": answer, "parent_tool_use_id": null, "user_message_uuid": input,
                        "user_message_uuids": [input], "message": {"role": "assistant", "content": [{"type": "text", "text": "Working on it"}]}, "session_id": host.session});
                    if let Some(usage) = self.usage(host, state, sent) {
                        message["message"]["usage"] = usage;
                    }
                    self.emit(host, state, "message", message);
                }
            }
        }
    }

    /// The last messages of a turn: Claude's answer and the result, or the API's error and the
    /// failed result the SDK ends it with.
    fn finish_turn(&self, host: &Host, state: &mut HostState, error: Option<ApiError>) {
        let Some(taken) = state.turn.take() else { return };
        let last = taken.last().cloned().unwrap_or_default();
        let (answer, result) = (self.uuid(), self.uuid());
        match &error {
            None => {
                let mut message = json!({"type": "assistant", "uuid": answer, "parent_tool_use_id": null, "user_message_uuid": last,
                    "user_message_uuids": taken, "message": {"role": "assistant", "content": [{"type": "text", "text": "Done"}]}, "session_id": host.session});
                if let Some(usage) = self.usage(host, state, 100) {
                    message["message"]["usage"] = usage;
                }
                self.emit(host, state, "message", message);
                self.emit(host, state, "result", json!({"type": "result", "subtype": "success", "uuid": result, "user_message_uuid": last, "user_message_uuids": taken,
                    "is_error": false, "stop_reason": "end_turn", "terminal_reason": "completed", "result": "Done", "session_id": host.session}));
            }
            Some(error) => {
                let limited = error.error == "rate_limit";
                if limited {
                    let (event, resets_at) = (self.uuid(), self.limit_resets_at());
                    self.emit(host, state, "status", json!({"type": "rate_limit_event", "uuid": event, "session_id": host.session,
                        "rate_limit_info": {"status": "rejected", "resetsAt": resets_at / 1000, "rateLimitType": "five_hour", "utilization": 1.0}}));
                }
                let unused = json!({"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
                    "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0}});
                let content: Vec<Value> = error.said.iter().map(|said| json!({"type": "text", "text": said})).collect();
                let mut message = json!({"type": "assistant", "uuid": answer, "parent_tool_use_id": null, "user_message_uuid": last,
                    "user_message_uuids": taken, "error": error.error, "session_id": host.session,
                    "message": {"role": "assistant", "model": "<synthetic>", "content": content, "usage": unused}});
                if let Some(detail) = &error.detail {
                    message["api_error"] = detail.as_str().into();
                }
                self.emit(host, state, "message", message);
                let terminal = if limited { "blocking_limit" } else { "api_error" };
                let mut payload = json!({"type": "result", "subtype": "success", "uuid": result, "user_message_uuid": last, "user_message_uuids": taken,
                    "is_error": true, "terminal_reason": terminal, "session_id": host.session});
                if let Some(status) = error.status {
                    payload["api_error_status"] = status.into();
                }
                self.emit(host, state, "result", payload);
            }
        }
        for input in &taken {
            let completed = self.uuid();
            self.emit(host, state, "status", json!({"type": "command_lifecycle", "state": "completed", "uuid": completed, "command_uuid": input, "session_id": host.session}));
        }
    }

    /// Journals an SDK observation as the host frames it and sends it on the lane, if one is open.
    fn emit(&self, host: &Host, state: &mut HostState, kind: &str, payload: Value) {
        let observation = json!({"type": "sdk", "kind": kind, "native": {}, "payload": payload, "failures": []});
        self.event(host, state, observation);
    }

    fn event(&self, host: &Host, state: &mut HostState, observation: Value) {
        state.cursor += 1;
        let frame = json!({"protocol_version": PROTOCOL_VERSION, "body": {"type": "event", "fence": host.bootstrap.fence, "execution": host.bootstrap.execution,
            "cursor": state.cursor, "observation": observation}});
        match serde_json::from_value::<Frame>(frame) {
            Ok(frame) => {
                state.journal.push_back((state.cursor, frame.clone()));
                send(state, &frame);
                self.tick();
            }
            Err(error) => self.troubled(format!("the host wrote an event that is not a frame: {error}")),
        }
    }

    /// Answers a command as the host does: a copy of one it answered gets the same receipt.
    fn command(&self, host: &Host, state: &mut HostState, command: Command) {
        let id = command.command_id.to_string();
        if let Some(receipt) = state.receipts.get(&id).cloned() {
            send(state, &receipt);
            return;
        }
        let digest = match command_digest(&command) {
            Ok(digest) => digest,
            Err(error) => return self.troubled(format!("a command the daemon sent has no digest: {error}")),
        };
        let receipt = |outcome: Value| -> Option<Frame> {
            let receipt = json!({"protocol_version": PROTOCOL_VERSION, "body": {"type": "receipt", "fence": host.bootstrap.fence, "execution": host.bootstrap.execution,
                "command_id": id, "command_sha256": digest, "outcome": outcome}});
            serde_json::from_value(receipt).map_err(|error| self.troubled(format!("the host wrote a receipt that is not a frame: {error}"))).ok()
        };
        let applied = json!({"status": "accepted", "phase": "applied"});
        let answered = |state: &mut HostState, outcome: Result<(), (&str, &str)>| {
            let outcome = match outcome {
                Ok(()) => applied.clone(),
                Err((code, message)) => json!({"status": "rejected", "code": code, "message": message}),
            };
            if let Some(receipt) = receipt(outcome) {
                state.receipts.insert(id.clone(), receipt.clone());
                send(state, &receipt);
                self.tick();
            }
        };
        match command.action {
            Action::Respond { request_id, epoch, kind, tool_use_id, decision, asked } => {
                let outcome = self.respond(host, state, Responded { request_id: request_id.0, epoch, kind, tool_use_id, decision, asked });
                return answered(state, outcome.map_err(|message| ("stale_request", message)));
            }
            Action::SetModel { model } => {
                if !OFFERED_MODELS.contains(&model.as_str()) {
                    return answered(state, Err(("unsupported_model", "Claude does not offer this model")));
                }
                lock(&self.0.controls).push(json!({"subtype": "set_model", "model": model}));
                return answered(state, Ok(()));
            }
            Action::SetPermissionMode { mode } => {
                let mode = match mode {
                    alondra_agent_protocol::PermissionMode::Default => "default",
                    alondra_agent_protocol::PermissionMode::Plan => "plan",
                    alondra_agent_protocol::PermissionMode::DontAsk => "dontAsk",
                };
                lock(&self.0.controls).push(json!({"subtype": "set_permission_mode", "mode": mode}));
                return answered(state, Ok(()));
            }
            Action::McpStatus => {
                lock(&self.0.controls).push(json!({"subtype": "mcp_status"}));
                self.emit(host, state, "control_result", json!({"control": "mcp_status", "command_id": id, "servers": [{"name": "fixture", "status": "connected"}]}));
                return answered(state, Ok(()));
            }
            _ => answered(state, Ok(())),
        }
        match command.action {
            Action::Start { input_id, prompt, .. } | Action::Offer { input_id, prompt } => self.offered(host, state, input_id.to_string(), prompt),
            Action::Interrupt | Action::ForceStop | Action::Close => {
                if matches!(command.action, Action::Interrupt) {
                    lock(&self.0.controls).push(json!({"subtype": "interrupt"}));
                }
                self.withdraw(host, state, matches!(command.action, Action::Interrupt));
                if state.turn.is_some() {
                    let taken = state.turn.take().unwrap_or_default();
                    let result = self.uuid();
                    let last = taken.last().cloned().unwrap_or_default();
                    self.emit(host, state, "result", json!({"type": "result", "subtype": "error_during_execution", "uuid": result, "user_message_uuid": last,
                        "user_message_uuids": taken, "is_error": true, "terminal_reason": "aborted_streaming", "session_id": host.session}));
                }
                if !matches!(command.action, Action::Interrupt) {
                    state.queued.clear();
                    self.event(host, state, json!({"type": "process_exited", "exit_code": 0, "signal": null}));
                    state.closing = true;
                }
            }
            _ => {}
        }
    }

    /// Answers a run of Claude's command line or of the host's probes from the world, and refuses
    /// anything else, which it counts as a leak.
    pub fn probe(&self, run: &ProbeRun) -> io::Result<ProbeOutput> {
        let args: Vec<String> = run.command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let environment: BTreeMap<String, String> = run
            .command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned())))
            .collect();
        let answer = |stdout: Value| Ok(ProbeOutput { success: true, stdout: stdout.to_string().into_bytes(), stderr: Vec::new() });
        if args.ends_with(&["auth".into(), "status".into(), "--json".into()]) {
            let account = account_of(&self.0.home, &environment);
            if lock(&self.0.world).signed_out.contains(&account) {
                return answer(json!({"loggedIn": false}));
            }
            return answer(json!({"loggedIn": true, "email": format!("{account}@example.com"), "subscriptionType": "max", "authMethod": "claude.ai"}));
        }
        if args.iter().any(|arg| arg == "--usage-probe") {
            let (held, ready) = &self.0.startup_probe;
            let mut held = lock(held);
            while *held {
                held = ready.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            drop(held);
            let session = match lock(&self.0.world).limit_resets_at {
                Some(resets_at) => json!({"kind": "five_hour", "used": 1.0, "resets_at": resets_at}),
                None => json!({"kind": "five_hour", "used": 0.2}),
            };
            return answer(json!({"plan": "max", "rate_limits_available": true, "windows": [session, {"kind": "seven_day", "used": 0.1}]}));
        }
        if args.iter().any(|arg| arg == "--model-probe") {
            return answer(json!({"native_default": MODEL, "models": [{"model": MODEL, "display_name": "Demo model", "reasoning_efforts": ["high"],
                "default_reasoning_effort": "high", "input_modalities": ["text"], "is_default": true}]}));
        }
        let what = std::iter::once(run.command.get_program().to_string_lossy().into_owned()).chain(args).collect::<Vec<_>>().join(" ");
        let what = format!("process {what}");
        lock(&self.0.leaks).push(what.clone());
        Err(io::Error::other(format!("the model runs no {what}")))
    }
}

/// The daemon's answer to a decision, with the request it names as the domain kept it.
struct Responded {
    request_id: String,
    epoch: u64,
    kind: DecisionKind,
    tool_use_id: Option<String>,
    decision: Decision,
    asked: serde_json::Map<String, Value>,
}

/// What a prompt asks the fake native Claude to decide: `decide:<scenario> <argument>`, also as
/// the user's words relayed with `[From <who>] `.
fn decision_scenario(prompt: &str) -> Option<&str> {
    let said = if prompt.starts_with("[From ") { prompt.split_once("] ")?.1 } else { prompt };
    said.strip_prefix("decide:")
}

/// The response the SDK hands Claude for `asked`: the decision as the host turns it into the SDK's
/// result, or a cancellation when no answer reached it.
fn sdk_response(asked: &Asked, decision: Option<Decision>) -> Value {
    if asked.kind == DecisionKind::Elicitation {
        return match decision {
            Some(Decision::Accept { content: Some(content) }) => json!({"action": "accept", "content": content}),
            Some(Decision::Accept { content: None }) => json!({"action": "accept"}),
            Some(Decision::Decline {}) => json!({"action": "decline"}),
            _ => json!({"action": "cancel"}),
        };
    }
    let mut response = match decision {
        Some(Decision::Allow { updated_input }) => json!({"behavior": "allow", "updatedInput": updated_input}),
        Some(Decision::Deny { message }) => json!({"behavior": "deny", "message": message}),
        _ => json!({"behavior": "deny", "message": CANCELLED}),
    };
    if asked.kind == DecisionKind::Plan && response["behavior"] == "allow" {
        response["updatedPermissions"] = json!([{"type": "setMode", "mode": "auto", "destination": "session"}]);
    }
    if let Some(tool_use_id) = &asked.tool_use_id {
        response["toolUseID"] = json!(tool_use_id);
    }
    response
}

fn send(state: &mut HostState, frame: &Frame) {
    if let Some(lane) = state.lane.as_mut()
        && lane.write(frame).is_err()
    {
        state.lane = None;
    }
}

impl Host {
    /// The first daemon attached to this host, or it ended before one did.
    fn attached(&self) {
        if self.unattached.swap(false, Ordering::SeqCst) {
            self.starting.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub fn alive(&self) -> bool {
        !lock(&self.state).ended && self.process.alive()
    }

    /// The host's process ends, its connections with it: by itself once it closed, or killed.
    pub fn end(&self, closed: bool) {
        let mut state = lock(&self.state);
        if std::mem::replace(&mut state.ended, true) {
            return;
        }
        self.attached();
        state.lane = None;
        let connections = std::mem::take(&mut state.connections);
        drop(state);
        self.process.end(closed);
        for connection in connections {
            let _ = connection.shutdown(std::net::Shutdown::Both);
        }
    }

    fn handshake(&self, reader: &mut FramedReader<UnixStream>, writer: &mut FramedWriter<UnixStream>) -> Result<(), String> {
        let b = &self.bootstrap;
        let wire = |error: alondra_agent_protocol::Error| error.to_string();
        writer
            .write(&Frame::new(Body::Hello(Hello {
                protocol_versions: vec![PROTOCOL_VERSION],
                capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
                fence: b.fence.clone(),
                host_release: b.manifest.host_release.clone(),
                host_sha256: b.manifest.host_sha256.clone(),
                sdk_version: b.manifest.sdk_version.clone(),
            })))
            .map_err(wire)?;
        writer
            .write(&Frame::new(Body::Challenge(Challenge {
                fence: b.fence.clone(),
                execution: b.execution.clone(),
                host_instance_id: b.host_instance_id.clone(),
                nonce: "a".repeat(64),
                executable_sha256: b.manifest.executable_sha256.clone(),
            })))
            .map_err(wire)?;
        let attach: Attach = match reader.read().map_err(wire)?.map(|frame| frame.body) {
            Some(Body::Attach(attach)) => attach,
            None => return Err(String::new()),
            other => return Err(format!("expected the daemon's attach, found {other:?}")),
        };
        verify_owner_proof(&self.secret, &attach).map_err(|e| format!("the daemon's proof: {e}"))?;
        let proof = host_proof(&self.secret, &attach).map_err(|e| e.to_string())?;
        writer
            .write(&Frame::new(Body::Attached(Attached {
                fence: attach.fence,
                execution: attach.execution,
                host_instance_id: attach.host_instance_id,
                owner_generation: attach.owner_generation,
                proof,
            })))
            .map_err(wire)
    }
}

fn serve(model: &ClaudeModel, host: &Arc<Host>, stream: UnixStream) {
    let Ok(clone) = stream.try_clone() else { return };
    let (mut reader, mut writer) = (FramedReader::new(stream), FramedWriter::new(clone));
    match host.handshake(&mut reader, &mut writer) {
        Ok(()) => host.attached(),
        Err(why) if why.is_empty() => return,
        Err(why) => return model.troubled(format!("the attach of the host of {}: {why}", host.task)),
    }
    {
        let mut state = lock(&host.state);
        let unacknowledged: Vec<Frame> = state.journal.iter().filter(|(cursor, _)| *cursor > state.acknowledged).map(|(_, frame)| frame.clone()).collect();
        state.lane = Some(writer);
        for frame in &unacknowledged {
            send(&mut state, frame);
        }
    }
    model.tick();
    while let Ok(Some(frame)) = reader.read() {
        model.tick();
        let mut state = lock(&host.state);
        if state.ended {
            return;
        }
        match frame.body {
            Body::Acknowledge(acknowledge) => {
                state.acknowledged = state.acknowledged.max(acknowledge.cursor);
                let acknowledged = state.acknowledged;
                state.journal.retain(|(cursor, _)| *cursor > acknowledged);
                if state.closing && state.acknowledged >= state.cursor {
                    drop(state);
                    host.end(true);
                    return;
                }
            }
            Body::Command(command) => model.command(host, &mut state, command),
            other => model.troubled(format!("the daemon sent the host of {} a frame it never sends: {other:?}", host.task)),
        }
    }
    lock(&host.state).lane = None;
    model.tick();
}

/// A doubled host of `engine-doubles` as the process of a host of the model.
struct Doubled(Served);
impl Process for Doubled {
    fn alive(&self) -> bool {
        self.0.alive()
    }
    fn end(&self, closed: bool) {
        self.0.end(ExitStatus::from_raw(if closed { 0 } else { KILLED }));
    }
}

/// The model plays every doubled host: the first connection to one starts its Claude from the
/// bootstrap it was launched with, and each connection is served on a thread of its own.
impl Serve for ClaudeModel {
    fn connect(&self, served: &Served, stream: UnixStream) {
        let host = match self.host(served.process_id()) {
            Some(host) => host,
            None => match self.start(served.bootstrap(), served.process_id(), Box::new(Doubled(served.clone()))) {
                Ok(host) => {
                    let (watched, ended) = (served.clone(), host.clone());
                    std::thread::spawn(move || {
                        watched.wait();
                        ended.end(false);
                    });
                    host
                }
                Err(error) => {
                    self.troubled(format!("a doubled host's bootstrap: {error}"));
                    return served.hold(stream);
                }
            },
        };
        if let Ok(connection) = stream.try_clone() {
            let mut state = lock(&host.state);
            if state.ended {
                return;
            }
            state.connections.push(connection);
        }
        let model = self.clone();
        self.spawn(move || model.serve(&host, stream));
    }

    fn stop(&self, served: &Served, stop: Stop) {
        served.end(stop.status());
    }
}
