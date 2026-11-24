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
//! or a process a test started for each host. The orchestrator's hosts have a model of their own
//! (`OrchestratorModel`), which plays the fake orchestrator Claude.

use alondra_agent_protocol as protocol;
use host_transport::{Bootstrap, Doubled, HostState, Identity, lock};
pub use host_transport::Process;
use protocol::{Action, Command, Decision, DecisionKind};
use alondra_engine_doubles::agent_host::{Serve, Served, Stop};
use alondra_engine_ports::agent_host::{ProbeOutput, ProbeRun};
use alondra_process_group::Member;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod host_transport;
mod fake_orchestrator;
pub use fake_orchestrator::OrchestratorModel;

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
/// What Claude Code says when the usage limit refuses a turn.
const LIMIT_TEXT: &str = "You've hit your limit";
/// What a small request costs, as the fake native Claude reports it.
static SMALL_USAGE: std::sync::LazyLock<Value> =
    std::sync::LazyLock::new(|| json!({"input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}));
/// What the SDK answers Claude for a request no answer reached.
const CANCELLED: &str = "Permission request cancelled";

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
    /// The API answers again the prompts scripted to be refused (`signed-out`, `overloaded`,
    /// `rate-limited`), as the fake native Claude does once the test drops `.recovered`.
    recovered: bool,
    /// Claude keeps working on the first prompt of each host and leaves the rest unread in that
    /// turn, as the fake native Claude does once the test drops `.hold`.
    hold: bool,
}

/// What went wrong between the daemon and the hosts, or something a host was asked it does not do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trouble(pub String);

/// A native Claude a host started: the session it runs, the one it resumed, how many prompts that
/// session had already, the folder and the command line and environment it got.
#[derive(Clone, Debug)]
pub struct NativeStart {
    /// The process id of the host that started it, which the native Claude runs under.
    pub process: u32,
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
        json!({"pid": self.process, "session": self.session, "resume": self.resume, "history": self.history, "cwd": self.cwd, "config": self.environment.get("CLAUDE_CONFIG_DIR")})
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
    /// Told of each frame either side wrote, after the host journaled it.
    moved: (Mutex<()>, std::sync::Condvar),
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
    /// The sessions Claude lost what it kept of, apart from the transcript in its profile.
    forgotten: Mutex<BTreeSet<String>>,
    /// What each native Claude got for each decision it asked for, as the fake native Claude
    /// records it.
    decisions: Mutex<Vec<Value>>,
    /// Every control request the native Claudes got, as the SDK sent it.
    controls: Mutex<Vec<Value>>,
    /// Beside this path Claude writes the plan it asks about, as the fake native Claude does
    /// beside its capture.
    capture: Mutex<Option<PathBuf>>,
    /// The processes Claude left running in the background, as the fake native Claude records
    /// them beside its capture.
    background: Mutex<Vec<Member>>,
}

pub struct StartupProbeGate(ClaudeModel);

impl Drop for StartupProbeGate {
    fn drop(&mut self) {
        *lock(&self.0.0.startup_probe.0) = false;
        self.0.0.startup_probe.1.notify_all();
    }
}

/// One host the daemon started: its process, its identity, and the conversation it holds.
pub type Host = host_transport::Host<Conversation>;
type State = HostState<Conversation>;

/// What a host's Claude holds: its turn, its queue and the decisions it waits on.
#[derive(Default)]
pub struct Conversation {
    /// The inputs the running turn took, in order.
    turn: Option<Vec<String>>,
    /// Inputs offered while a turn ran, by native id with their prompt, which Claude reads in the
    /// turn or takes as the next one.
    queued: VecDeque<(String, String)>,
    /// The prompt of every input the host was offered, by native id.
    prompts: BTreeMap<String, String>,
    /// Claude's session runs nothing.
    busy: bool,
    /// What the first request of this host's session wrote to the prompt cache, once it answered.
    first_written: Option<u64>,
    /// What the daemon answered each permission Claude asked, by request: `allow` or `deny`.
    answers: BTreeMap<String, String>,
    /// The decisions Claude waits on, by request.
    asked: BTreeMap<String, Asked>,
    /// How many decisions this host's Claude asked for: each names its request and tool use.
    decisions: u64,
    own_turn: OwnTurn,
    /// The native Claude runs: the host starts it on `start`, or on its first `offer` when an
    /// earlier host of the execution ended.
    native: bool,
    /// The native Claude exited: nothing reads what the host is offered.
    native_ended: bool,
    /// The native Claude hangs on a prompt: it writes nothing at all from then on.
    hung: bool,
    /// How the native Claude this host started takes its prompts, fixed when it started.
    turns: Option<Turns>,
    /// The doubled host this host runs as, which runs the processes Claude leaves in the
    /// background.
    doubled: Option<Served>,
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
    then: Then,
}

/// What Claude does once a decision it waits on is answered.
#[derive(Clone, PartialEq, Eq)]
enum Then {
    /// The turn the test plays goes on until the test ends it.
    GoOn,
    /// Claude ends the turn, as the fake native Claude does.
    Conclude,
    /// Claude's own turn asked to leave plan mode with this plan: a long one refused is asked again
    /// shorter, and otherwise the turn ends and Claude takes the input that waited behind it.
    OwnPlan(String),
}

/// A turn Claude opens by itself, as when a background subagent hands its report back.
#[derive(Default, PartialEq, Eq)]
enum OwnTurn {
    #[default]
    None,
    /// The turn starts when the test says (`ClaudeModel::own_turn`).
    Later,
    /// The turn starts as the next prompt arrives, which waits queued behind it.
    Next,
    /// The turn runs, with these inputs waiting behind it.
    Running(Vec<String>),
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
            moved: (Mutex::new(()), std::sync::Condvar::new()),
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
            forgotten: Mutex::default(),
            decisions: Mutex::default(),
            controls: Mutex::default(),
            capture: Mutex::default(),
            background: Mutex::default(),
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

    /// Claude keeps working on each host's first prompt from now on, and leaves the rest unread.
    pub fn hold(&self) {
        lock(&self.0.world).hold = true;
    }

    /// The API answers again the prompts scripted to be refused.
    pub fn recover(&self) {
        lock(&self.0.world).recovered = true;
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
    fn usage(&self, host: &Host, state: &mut State, sent: u64) -> Option<Value> {
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

    /// The processes Claude left running in the background, in the order they started: a command
    /// it waits on (`background-command`) replaces what was there, as the fake native Claude
    /// records them.
    pub fn background(&self) -> Vec<Member> {
        lock(&self.0.background).clone()
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
        self.0.starting.load(Ordering::SeqCst) == 0 && lock(&self.0.hosts).iter().all(|host| host.settled())
    }

    /// Claude loses what it kept of `session`: a host that resumes it starts with no history.
    pub fn forget(&self, session: &str) {
        lock(&self.0.sessions).remove(&session.to_lowercase());
        lock(&self.0.forgotten).insert(session.to_lowercase());
    }

    /// The process of `task`'s host dies, with whatever it started.
    pub fn kill(&self, task: &str) {
        if let Some(host) = self.running(task) {
            host.end(false);
        }
    }

    /// Whether the host whose process goes by `process_id` still runs.
    pub fn runs(&self, process_id: u32) -> bool {
        self.host(process_id).is_some_and(|host| host.alive())
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
        let (bootstrap, secret) = Bootstrap::read(bootstrap)?;
        let Some(task) = bootstrap.task() else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the model plays no orchestrator host"));
        };
        let identity = Identity {
            account: account_of(&self.0.home, &bootstrap.native.env),
            capability: bootstrap.native.env.get("ALONDRA_DOMAIN_CAPABILITY").cloned().unwrap_or_default(),
            session: bootstrap.native.session_id.clone(),
            resumes: bootstrap.resumes(),
            process_id,
            task,
        };
        let host = Arc::new(Host::new(identity, (bootstrap, secret), CAPABILITIES, process, self.0.starting.clone(), Conversation::default()));
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

    /// The host starts its native Claude: on the session's transcript so far when it resumes it. A
    /// session Claude lost cannot be resumed: Claude exits at once and says why.
    fn open_native(&self, host: &Host, state: &mut State) {
        state.native = true;
        state.turns = Some(lock(&self.0.world).turns);
        let session = host.session.to_lowercase();
        let history = lock(&self.0.sessions).get(&session).copied().unwrap_or_default();
        if host.resumes && lock(&self.0.forgotten).contains(&session) {
            state.native_ended = true;
            let said = format!("RuntimeError: No conversation found with session ID: {session}\n");
            self.event(host, state, json!({"type": "sdk", "kind": "diagnostic", "native": {}, "payload": {"source": "stderr", "text": said, "truncated_at_bytes": 16_384}, "failures": []}));
            self.event(host, state, json!({"type": "process_exited", "exit_code": 1, "signal": null}));
            return;
        }
        lock(&self.0.starts).push(NativeStart {
            process: host.process_id,
            resume: host.resumes.then(|| session.clone()),
            session,
            history,
            cwd: host.bootstrap.native.cwd.clone(),
            arguments: host.bootstrap.arguments(),
            environment: host.bootstrap.native.env.clone(),
        });
    }

    /// Serves one daemon's connection to `host`: the attach, what it had not acknowledged, then
    /// every frame it sends, until it goes. It returns once the connection closes.
    pub fn serve(&self, host: &Arc<Host>, stream: UnixStream) {
        host_transport::serve(self, host, stream);
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
        let _moved = lock(&self.0.moved.0);
        self.0.moved.1.notify_all();
    }

    /// Waits until the journal the host in `state_dir` keeps in its outbox `holds` what a test
    /// looks for, read again each time a host journals an event or a receipt: a host journals each
    /// frame before it sends it, so the daemon reading one says nothing of the next.
    pub fn until_journaled(&self, state_dir: &Path, holds: impl Fn(&str) -> bool) {
        let outbox = state_dir.join("outbox");
        let mut moved = lock(&self.0.moved.0);
        while !holds(&String::from_utf8_lossy(&std::fs::read(&outbox).unwrap_or_default())) {
            moved = self.0.moved.1.wait(moved).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
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
        let asked = Asked { kind: DecisionKind::Permission, tool: Some("Bash".into()), tool_use_id: Some(tool_use), payload, epoch: state.wire.cursor,
            taken: state.turn.clone().unwrap_or_default(), then: Then::GoOn };
        state.asked.insert(request.to_owned(), asked);
        Ok(())
    }

    /// What the daemon answered the permission `request` of `task`'s node, if it has.
    pub fn answer_of(&self, task: &str, request: &str) -> Option<String> {
        self.hosts_of(task).iter().find_map(|host| lock(&host.state).answers.get(request).cloned())
    }

    /// An input reaches the host, and Claude takes it as `Turns` says.
    fn offered(&self, host: &Host, state: &mut State, input: String, prompt: String) {
        let turns = state.turns.unwrap_or_else(|| lock(&self.0.world).turns);
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
    fn queue(&self, host: &Host, state: &mut State, input: String, prompt: String) {
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
    /// it has no script for. The prompts the fake native Claude has a script for go as they go
    /// there. The prompt goes into the session's transcript in Claude's profile, which the daemon
    /// reads before it resumes the session.
    fn answer(&self, host: &Host, state: &mut State, input: String, prompt: &str) {
        let session = host.session.to_lowercase();
        self.transcribe(host, &input, prompt);
        if state.hung {
            return;
        }
        if !host.resumes && prompt.ends_with("refused-by-claude") {
            return self.lifecycle(host, state, &input, "refused");
        }
        if !host.resumes && prompt.ends_with("hung-silent") {
            state.hung = true;
            return;
        }
        if prompt.ends_with("complete-unstarted") {
            for lifecycle in ["queued", "completed"] {
                self.lifecycle(host, state, &input, lifecycle);
            }
            return;
        }
        let mut taken = state.turn.take().unwrap_or_default();
        taken.push(input.clone());
        let wake = prompt.starts_with("Your turn ended without a result") && taken.len() == 1;
        if state.own_turn == OwnTurn::Next && !wake {
            return self.own_turn_started(host, state, taken);
        }
        for lifecycle in ["queued", "started"] {
            self.lifecycle(host, state, &input, lifecycle);
        }
        if taken.len() == 1 {
            let (title, init) = (self.uuid(), self.uuid());
            self.emit(host, state, "status", json!({"type": "system", "subtype": "session_title_changed", "title": "fixture", "session_id": session, "uuid": title}));
            self.emit(host, state, "initialized", self.init(host, &init, "default"));
            for subtype in ["dev_intent", "future_subtype"] {
                let uuid = self.uuid();
                self.emit(host, state, "status", json!({"type": "system", "subtype": subtype, "session_id": session, "uuid": uuid}));
            }
        }
        if wake {
            return self.conclude(host, state, &input, &taken);
        }
        if taken.len() == 1 && let Some(said) = decision_scenario(prompt) {
            return self.decide(host, state, taken, said);
        }
        if let Some(refusal) = refusal(prompt) {
            if lock(&self.0.world).recovered {
                self.consume(host, state, &input, &taken, json!({"type": "text", "text": "Fixture answer"}));
                return self.conclude(host, state, &input, &taken);
            }
            state.turn = Some(taken);
            return self.finish_turn(host, state, Some(refusal));
        }
        let resumed = host.resumes;
        let consumed = json!({"type": "text", "text": "Fixture consumed input"});
        match prompt {
            "consume-and-hold" => {
                self.consume(host, state, &input, &taken, consumed);
                state.turn = Some(taken);
            }
            "failed-delivery" => self.failed(host, state, &input, &taken, "error_during_execution", "Fixture delivery failed before consumption"),
            _ if prompt.ends_with("max-turns") => {
                self.consume(host, state, &input, &taken, consumed);
                self.failed(host, state, &input, &taken, "error_max_turns", "Fixture turn reached its limit");
            }
            _ if prompt.ends_with("rate-limited-twice") => self.limited_twice(host, state, taken),
            _ if !resumed && (prompt.ends_with("unread") || prompt.ends_with("stall-before-starting")) => state.turn = Some(taken),
            _ if !resumed && prompt.ends_with("running-then-silent") => {
                let running = self.uuid();
                self.emit(host, state, "status", json!({"type": "system", "subtype": "session_state_changed", "state": "running", "session_id": session, "uuid": running}));
                self.lifecycle(host, state, &input, "started");
                state.turn = Some(taken);
            }
            "consume-and-native-lifecycle" => {
                self.consume(host, state, &input, &taken, consumed);
                for lifecycle in ["queued", "started", "completed"] {
                    self.lifecycle(host, state, "30000000-0000-4000-8000-000000000001", lifecycle);
                }
                state.turn = Some(taken);
            }
            _ if lock(&self.0.world).hold => {
                if taken.len() == 1 {
                    self.consume(host, state, &input, &taken, consumed);
                }
                state.turn = Some(taken);
            }
            "large-context" | "large-context-unsized" => {
                let usage = json!({"input_tokens": 3, "output_tokens": 400, "cache_creation_input_tokens": 2000, "cache_read_input_tokens": 248_000,
                    "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 2000}});
                let answer = self.uuid();
                self.emit(host, state, "message", json!({"type": "assistant", "session_id": session, "uuid": answer, "parent_tool_use_id": null,
                    "message": {"id": format!("msg_{answer}"), "type": "message", "role": "assistant", "model": "fixture", "content": [{"type": "text", "text": "Fixture answer"}],
                        "stop_reason": null, "stop_sequence": null, "usage": usage},
                    "user_message_uuid": input, "user_message_uuids": taken}));
                let sized = if prompt == "large-context" {
                    json!({"fixture": {"inputTokens": 3, "outputTokens": 400, "contextWindow": 1_000_000}})
                } else {
                    json!({"fixture-subagent": {"inputTokens": 1, "outputTokens": 1, "contextWindow": 200_000}})
                };
                self.conclude_with(host, state, &input, &taken, sized);
            }
            "huge-tool-result" => {
                self.consume(host, state, &input, &taken, json!({"type": "tool_use", "id": "toolu_fixture_huge", "name": "Bash", "input": {"command": "cat huge.log"}}));
                let (uuid, output) = (self.uuid(), "x".repeat(5 * 1024 * 1024));
                self.emit(host, state, "message", json!({"type": "user", "session_id": session, "uuid": uuid, "parent_tool_use_id": null,
                    "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_fixture_huge", "content": output}]},
                    "tool_use_result": {"stdout": output, "stderr": "", "interrupted": false}}));
                self.conclude(host, state, &input, &taken);
            }
            _ if prompt.starts_with("bulky:") => {
                for _ in 0..prompt["bulky:".len()..].parse::<usize>().unwrap_or_default() {
                    self.consume(host, state, &input, &taken, json!({"type": "text", "text": "x".repeat(900_000)}));
                }
                self.conclude(host, state, &input, &taken);
            }
            "background" => {
                self.consume(host, state, &input, &taken, consumed);
                self.emit(host, state, "task", json!({"type": "system", "subtype": "task_started", "task_id": "fixture-background", "description": "Fixture owned work",
                    "task_type": "local_agent", "uuid": "00000000-0000-4000-8000-000000000101", "session_id": session}));
                state.turn = Some(taken);
            }
            _ if prompt.ends_with("background-group") => {
                let Some(served) = self.doubled(host, state) else { return };
                let shell = served.start_descendant(None, true);
                let child = served.start_descendant(Some(&shell), false);
                lock(&self.0.background).extend([shell, child]);
                self.background_bash(host, state, &input, &taken, ("toolu_fixture_background_group", "stress.sh", "bfixture2"));
            }
            _ if prompt.ends_with("background-shell") || prompt.contains("background-shell\n") => {
                let Some(served) = self.doubled(host, state) else { return };
                lock(&self.0.background).push(served.start_descendant(None, false));
                self.background_bash(host, state, &input, &taken, ("toolu_fixture_background_shell", "bazel test //app:snapshots", "bfixture1"));
            }
            _ if prompt.ends_with("background-command") => self.background_command(host, state, &input, &taken, true),
            _ if prompt.ends_with("background-command-silent") => self.background_command(host, state, &input, &taken, false),
            "own-turn-later" | "own-turn-plan" => {
                self.consume(host, state, &input, &taken, consumed);
                self.conclude(host, state, &input, &taken);
                state.own_turn = if prompt == "own-turn-later" { OwnTurn::Later } else { OwnTurn::Next };
            }
            _ => {
                if taken.len() > 1 {
                    let notification = self.uuid();
                    self.emit(host, state, "task", json!({"type": "system", "subtype": "task_notification", "task_id": "fixture-background", "status": "completed",
                        "output_file": "fixture-output", "summary": "Fixture work completed", "uuid": notification, "session_id": session}));
                }
                self.conclude(host, state, &input, &taken);
            }
        }
    }

    /// The doubled host `host` runs as, which a process Claude leaves running needs.
    fn doubled(&self, host: &Host, state: &State) -> Option<Served> {
        let served = state.doubled.clone();
        if served.is_none() {
            self.troubled(format!("the host of {} is no doubled host: nothing runs what Claude leaves in the background", host.task));
        }
        served
    }

    /// Claude ran `(tool_use_id, command, task_id)` as a Bash command in the background, as the
    /// fake native Claude does: the task starts and the turn ends saying it goes on.
    fn background_bash(&self, host: &Host, state: &mut State, input: &str, taken: &[String], (tool_use_id, command, task_id): (&str, &str, &str)) {
        self.consume(host, state, input, taken, json!({"type": "tool_use", "id": tool_use_id, "name": "Bash", "input": {"command": command, "run_in_background": true}}));
        let started = self.uuid();
        self.emit(host, state, "task", json!({"type": "system", "subtype": "task_started", "task_id": task_id, "tool_use_id": tool_use_id, "description": command,
            "task_type": "local_bash", "is_backgrounded": true, "uuid": started, "session_id": host.session.to_lowercase()}));
        self.conclude(host, state, input, taken);
    }

    /// Claude left a command running in the background that ends once the test releases it
    /// (`ClaudeModel::background`, `InProcessHosts::end_descendant`), as the fake native Claude
    /// does for `background-command`. Its notification, which names no run, opens a turn of
    /// Claude's own when `opens_turn`, and otherwise waits for the next turn to read it.
    fn background_command(&self, host: &Host, state: &mut State, input: &str, taken: &[String], opens_turn: bool) {
        let Some(served) = self.doubled(host, state) else { return };
        let command = served.start_descendant(None, false);
        *lock(&self.0.background) = vec![command];
        self.background_bash(host, state, input, taken, ("toolu_fixture_background", "read line < release", "fixture-command"));
        let (model, process_id) = (self.clone(), host.process_id);
        self.spawn(move || {
            let completed = served.wait_descendant(&command).is_some_and(|status| status.success());
            let Some(host) = model.host(process_id) else { return };
            let mut state = lock(&host.state);
            if served.alive() && !state.wire.ended {
                model.background_ended(&host, &mut state, completed, opens_turn);
            }
        });
    }

    fn background_ended(&self, host: &Host, state: &mut State, completed: bool, opens_turn: bool) {
        let session = host.session.to_lowercase();
        let notification = self.uuid();
        self.emit(host, state, "task", json!({"type": "system", "subtype": "task_notification", "task_id": "fixture-command", "tool_use_id": "toolu_fixture_background",
            "status": if completed { "completed" } else { "failed" }, "output_file": "fixture-output", "summary": "read line < release", "uuid": notification, "session_id": session}));
        if !opens_turn {
            return;
        }
        let (init, answer, result) = (self.uuid(), self.uuid(), self.uuid());
        self.emit(host, state, "initialized", self.init(host, &init, "default"));
        self.emit(host, state, "message", json!({"type": "assistant", "session_id": session, "uuid": answer, "parent_tool_use_id": null,
            "message": {"id": format!("msg_{answer}"), "type": "message", "role": "assistant", "model": "fixture", "content": [{"type": "text", "text": "The command ended"}],
                "stop_reason": null, "stop_sequence": null, "usage": SMALL_USAGE.clone()}}));
        self.emit(host, state, "result", json!({"type": "result", "subtype": "success", "is_error": false, "session_id": session, "uuid": result,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "result": "The command ended", "terminal_reason": "completed", "origin": {"kind": "task-notification", "producer": "session-task"}}));
    }

    fn lifecycle(&self, host: &Host, state: &mut State, input: &str, lifecycle: &str) {
        let uuid = self.uuid();
        self.emit(host, state, "status", json!({"type": "command_lifecycle", "command_uuid": input, "state": lifecycle, "session_id": host.session.to_lowercase(), "uuid": uuid}));
    }

    fn init(&self, host: &Host, uuid: &str, permission_mode: &str) -> Value {
        json!({"type": "system", "subtype": "init", "session_id": host.session.to_lowercase(), "uuid": uuid, "claude_code_version": "2.1.300-fixture",
            "model": "fixture", "cwd": host.bootstrap.native.cwd, "tools": ["Bash"], "mcp_servers": [], "permissionMode": permission_mode, "slash_commands": [],
            "output_style": "default", "skills": [], "plugins": [], "apiKeySource": "none"})
    }

    /// Claude answers in the turn that took `taken` with `block`, and the turn goes on.
    fn consume(&self, host: &Host, state: &mut State, input: &str, taken: &[String], block: Value) {
        let consumed = self.uuid();
        self.emit(host, state, "message", json!({"type": "assistant", "session_id": host.session.to_lowercase(), "uuid": consumed, "parent_tool_use_id": null,
            "message": {"id": format!("msg_{consumed}"), "type": "message", "role": "assistant", "model": "fixture", "content": [block],
                "stop_reason": null, "stop_sequence": null, "usage": SMALL_USAGE.clone()},
            "user_message_uuid": input, "user_message_uuids": taken}));
    }

    /// The turn that took `taken` ends as failed with `subtype`, and every input it took completes.
    fn failed(&self, host: &Host, state: &mut State, input: &str, taken: &[String], subtype: &str, error: &str) {
        let result = self.uuid();
        let turns = u8::from(subtype != "error_during_execution");
        self.emit(host, state, "result", json!({"type": "result", "subtype": subtype, "is_error": true, "session_id": host.session.to_lowercase(), "uuid": result,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": turns, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "user_message_uuid": input, "errors": [error]}));
        for input in taken {
            self.lifecycle(host, state, input, "completed");
        }
    }

    /// The usage limit ends the turn, and then the turn Claude opens by itself when its subagent
    /// hands its report back.
    fn limited_twice(&self, host: &Host, state: &mut State, taken: Vec<String>) {
        state.turn = Some(taken);
        self.finish_turn(host, state, Some(scripted_refusal(429)));
        let session = host.session.to_lowercase();
        let (init, answer, result) = (self.uuid(), self.uuid(), self.uuid());
        self.emit(host, state, "initialized", self.init(host, &init, "default"));
        self.emit(host, state, "message", json!({"type": "assistant", "session_id": session, "uuid": answer, "parent_tool_use_id": null, "error": "rate_limit",
            "message": {"id": format!("msg_{answer}"), "type": "message", "role": "assistant", "model": "fixture", "content": [{"type": "text", "text": LIMIT_TEXT}],
                "stop_reason": null, "stop_sequence": null, "usage": SMALL_USAGE.clone()}}));
        self.emit(host, state, "result", json!({"type": "result", "subtype": "success", "is_error": true, "api_error_status": 429, "session_id": session, "uuid": result,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "result": LIMIT_TEXT, "terminal_reason": "api_error", "stop_reason": null, "origin": {"kind": "task-notification", "producer": "session-task"}}));
    }

    /// Claude opens a turn of its own on the hosts whose last turn was scripted with
    /// `own-turn-later`, as when a subagent hands its report back while the task rests.
    pub fn own_turn(&self) {
        let hosts = lock(&self.0.hosts).clone();
        for host in hosts {
            let mut state = lock(&host.state);
            if state.own_turn != OwnTurn::Later {
                continue;
            }
            state.own_turn = OwnTurn::None;
            let (init, answer) = (self.uuid(), self.uuid());
            self.emit(&host, &mut state, "initialized", self.init(&host, &init, "default"));
            self.emit(&host, &mut state, "message", json!({"type": "assistant", "session_id": host.session.to_lowercase(), "uuid": answer, "parent_tool_use_id": null,
                "message": {"id": format!("msg_{answer}"), "type": "message", "role": "assistant", "model": "fixture", "content": [{"type": "text", "text": "The report is in"}],
                    "stop_reason": null, "stop_sequence": null, "usage": SMALL_USAGE.clone()}}));
        }
    }

    /// Claude's own turn starts as `taken` arrives, which waits queued behind it, and asks to leave
    /// plan mode with a long plan.
    fn own_turn_started(&self, host: &Host, state: &mut State, taken: Vec<String>) {
        let input = taken.last().cloned().unwrap_or_default();
        self.lifecycle(host, state, &input, "queued");
        let init = self.uuid();
        self.emit(host, state, "initialized", self.init(host, &init, "plan"));
        state.own_turn = OwnTurn::Running(taken);
        self.own_plan(host, state, long_plan());
    }

    fn own_plan(&self, host: &Host, state: &mut State, plan: String) {
        state.decisions += 1;
        let (request_id, tool_use_id) = (format!("decision-{}", state.decisions), format!("toolu_fixture_{}", state.decisions));
        let file = format!("{}.plan.md", self.capture().display());
        if let Err(error) = std::fs::write(&file, &plan) {
            self.troubled(format!("the plan of {}: {error}", host.task));
        }
        let answer = self.uuid();
        self.emit(host, state, "message", json!({"type": "assistant", "session_id": host.session.to_lowercase(), "uuid": answer, "parent_tool_use_id": null,
            "message": {"id": format!("msg_{answer}"), "type": "message", "role": "assistant", "model": "fixture",
                "content": [{"type": "tool_use", "id": tool_use_id, "name": "ExitPlanMode", "input": {}}], "stop_reason": null, "stop_sequence": null, "usage": SMALL_USAGE.clone()}}));
        let cwd = std::fs::canonicalize(&host.bootstrap.native.cwd).unwrap_or_else(|_| host.bootstrap.native.cwd.clone()).display().to_string();
        let payload = json!({"tool": "ExitPlanMode", "input": {"plan": plan, "planFilePath": file}, "request": {"toolUseID": tool_use_id, "requestId": request_id}, "cwd": cwd});
        self.event(host, state, json!({"type": "decision_requested", "request_id": request_id, "kind": DecisionKind::Plan, "payload": payload, "tool_use_id": tool_use_id}));
        let Value::Object(payload) = payload else { return };
        let asked = Asked { kind: DecisionKind::Plan, tool: Some("ExitPlanMode".into()), tool_use_id: Some(tool_use_id), payload, epoch: state.wire.cursor, taken: Vec::new(), then: Then::OwnPlan(plan) };
        state.asked.insert(request_id, asked);
    }

    /// Claude's own turn ends, and Claude takes the inputs that waited behind it in a turn that
    /// ends at once.
    fn own_turn_ended(&self, host: &Host, state: &mut State) {
        let OwnTurn::Running(taken) = std::mem::take(&mut state.own_turn) else { return };
        let (result, unnamed) = (self.uuid(), self.uuid());
        self.emit(host, state, "result", json!({"type": "result", "subtype": "success", "is_error": false, "session_id": host.session.to_lowercase(), "uuid": result,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "result": "Fixture own turn", "terminal_reason": "completed", "user_message_uuid": unnamed}));
        let input = taken.last().cloned().unwrap_or_default();
        self.lifecycle(host, state, &input, "started");
        self.consume(host, state, &input, &taken, json!({"type": "text", "text": "Fixture consumed input"}));
        self.conclude(host, state, &input, &taken);
    }

    /// The turn that took `taken` ends with a result for `input`, and every input it took completes.
    fn conclude(&self, host: &Host, state: &mut State, input: &str, taken: &[String]) {
        self.conclude_with(host, state, input, taken, json!({}));
    }

    /// The turn ends as `conclude` ends it, its result saying what each model used.
    fn conclude_with(&self, host: &Host, state: &mut State, input: &str, taken: &[String], model_usage: Value) {
        let session = host.session.to_lowercase();
        let result = self.uuid();
        self.emit(host, state, "result", json!({"type": "result", "subtype": "success", "is_error": false, "session_id": session, "uuid": result,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": model_usage, "permission_denials": [],
            "user_message_uuid": input, "result": "Fixture result", "terminal_reason": "completed", "user_message_uuids": taken}));
        for input in taken {
            let completed = self.uuid();
            self.emit(host, state, "status", json!({"type": "command_lifecycle", "command_uuid": input, "state": "completed", "session_id": session, "uuid": completed}));
        }
    }

    /// Claude calls a tool its rules do not allow, or an MCP server asks for input, as the fake
    /// native Claude does for `decide:<scenario> <argument>`; the host asks the daemon and the
    /// turn waits for the answer.
    fn decide(&self, host: &Host, state: &mut State, taken: Vec<String>, said: &str) {
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
        let asked = Asked { kind, tool, tool_use_id, payload, epoch: state.wire.cursor, taken: taken.clone(), then: Then::Conclude };
        state.asked.insert(request_id, asked);
        state.turn = Some(taken);
    }

    /// Where Claude writes what the fake native Claude writes beside its capture.
    fn capture(&self) -> PathBuf {
        lock(&self.0.capture).clone().unwrap_or_else(|| self.0.home.join("fake-claude-capture"))
    }

    /// The daemon's answer reaches the request Claude waits on under exactly this identity, as the
    /// host applies it; the SDK hands Claude its response and Claude goes on with its turn.
    fn respond(&self, host: &Host, state: &mut State, respond: Responded) -> Result<(), &'static str> {
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
        if waiting.then == Then::GoOn {
            return Ok(());
        }
        if let Some(tool_use_id) = &waiting.tool_use_id {
            let allowed = response["behavior"] == "allow";
            let content = if allowed { response["updatedInput"].to_string() } else { response["message"].as_str().unwrap_or("denied").to_owned() };
            let uuid = self.uuid();
            self.emit(host, state, "message", json!({"type": "user", "session_id": host.session.to_lowercase(), "uuid": uuid, "parent_tool_use_id": null,
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_use_id, "is_error": !allowed, "content": content}]}}));
        }
        if let Then::OwnPlan(plan) = &waiting.then {
            if response["behavior"] != "allow" && plan == &long_plan() {
                self.own_plan(host, state, FIXTURE_PLAN.to_owned());
            } else {
                self.own_turn_ended(host, state);
            }
            return Ok(());
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
    fn withdraw(&self, host: &Host, state: &mut State, answered: bool) {
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
    fn next_turn(&self, host: &Host, state: &mut State) {
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
    fn finish_turn(&self, host: &Host, state: &mut State, error: Option<ApiError>) {
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
    fn emit(&self, host: &Host, state: &mut State, kind: &str, payload: Value) {
        let (failures, native) = (failures(&payload), native_ids(&payload));
        let observation = json!({"type": "sdk", "kind": kind, "native": native, "payload": payload, "failures": failures});
        self.event(host, state, observation);
    }

    fn event(&self, host: &Host, state: &mut State, observation: Value) {
        match state.wire.event(&host.bootstrap, observation) {
            Ok(()) => self.tick(),
            Err(why) => self.troubled(why),
        }
    }

    /// Answers a command as the host does.
    fn command(&self, host: &Host, state: &mut State, command: Command) {
        let id = command.command_id.to_string();
        let answered = |state: &mut State, outcome: Result<(), (&str, &str)>| {
            let outcome = match outcome {
                Ok(()) => json!({"status": "accepted", "phase": "applied"}),
                Err((code, message)) => json!({"status": "rejected", "code": code, "message": message}),
            };
            match state.wire.receipt(&host.bootstrap, &command, outcome) {
                Ok(()) => self.tick(),
                Err(why) => self.troubled(why),
            }
        };
        match command.action.clone() {
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
        let starts = matches!(command.action, Action::Start { .. });
        match command.action.clone() {
            Action::Start { input_id, prompt, .. } | Action::Offer { input_id, prompt } => {
                if !state.native && (starts || host.bootstrap.journal_base.is_some()) {
                    self.open_native(host, state);
                }
                if !state.native_ended {
                    self.offered(host, state, input_id.to_string(), prompt);
                }
            }
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
                    state.wire.closing = true;
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

/// What the host reports as failed in an SDK message, as its classifier does.
fn failures(payload: &Value) -> Vec<Value> {
    let mut failures = Vec::new();
    let mut failure = |surface: &str, code: &Value| {
        let code = code.as_str().map_or_else(|| code.to_string(), str::to_owned);
        failures.push(json!({"surface": surface, "code": code, "details": payload}));
    };
    let is_error = &payload["is_error"];
    match payload["type"].as_str() {
        Some("result") => {
            if !payload["startup_failure_reason"].is_null() {
                failure("startup", &payload["startup_failure_reason"]);
            }
            if payload["subtype"] != "success" || *is_error != Value::Bool(false) {
                let code = if payload["subtype"] == "success" { json!("success_is_error") } else { payload["subtype"].clone() };
                failure("result", &code);
            }
            let terminal = &payload["terminal_reason"];
            let settled = ["completed", "background_requested", "tool_deferred"].iter().any(|reason| terminal == reason);
            if !terminal.is_null() && (*is_error != Value::Bool(false) || !settled) {
                failure("terminal", terminal);
            }
        }
        Some("assistant") if !payload["error"].is_null() => failure("assistant", &payload["error"]),
        Some("rate_limit_event") if payload["rate_limit_info"]["status"] == "rejected" => failure("rate_limit", &json!("rejected")),
        _ => {}
    }
    failures
}

/// The native ids the host lifts out of an SDK message.
fn native_ids(payload: &Value) -> Value {
    let pairs = [("message_id", "uuid"), ("turn_id", "turn_id"), ("tool_use_id", "tool_use_id"), ("parent_tool_use_id", "parent_tool_use_id"), ("task_id", "task_id"), ("run_id", "run_id")];
    Value::Object(pairs.iter().filter_map(|(field, source)| payload[*source].as_str().map(|id| ((*field).to_owned(), json!(id)))).collect())
}

/// What the API answers a turn the fake native Claude refuses, `signed-out`, `overloaded` or
/// `rate-limited`.
fn refusal(prompt: &str) -> Option<ApiError> {
    [("signed-out", 401), ("overloaded", 529), ("rate-limited", 429)].into_iter()
        .find(|(script, _)| prompt.ends_with(script)).map(|(_, status)| scripted_refusal(status))
}

fn scripted_refusal(status: u16) -> ApiError {
    let (error, said) = match status {
        401 => ("authentication_failed", "Failed to authenticate. API Error: 401 OAuth token has expired"),
        429 => ("rate_limit", LIMIT_TEXT),
        _ => ("server_error", "API Error: 529 Overloaded"),
    };
    ApiError { error: error.into(), status: Some(status), detail: None, said: Some(said.into()) }
}

/// The plan Claude's own turn first asks to leave plan mode with: longer than Alondra takes.
fn long_plan() -> String {
    format!("# Own plan\n\n{}", (1..42).map(|point| format!("- [ ] Point {point}")).collect::<Vec<_>>().join("\n"))
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

/// The model plays every doubled host: the first connection to one starts its Claude from the
/// bootstrap it was launched with, and each connection is served on a thread of its own.
impl host_transport::Model for ClaudeModel {
    type Play = Conversation;

    fn tick(&self) {
        ClaudeModel::tick(self);
    }

    fn troubled(&self, why: String) {
        ClaudeModel::troubled(self, why);
    }

    fn command(&self, host: &Host, state: &mut State, command: Command) {
        ClaudeModel::command(self, host, state, command);
    }
}

impl Serve for ClaudeModel {
    fn connect(&self, served: &Served, stream: UnixStream) {
        let host = match self.host(served.process_id()) {
            Some(host) => host,
            None => match self.start(served.bootstrap(), served.process_id(), Box::new(Doubled(served.clone()))) {
                Ok(host) => {
                    host_transport::ends_with(served, &host);
                    lock(&host.state).doubled = Some(served.clone());
                    host
                }
                Err(error) => {
                    self.troubled(format!("a doubled host's bootstrap: {error}"));
                    return served.hold(stream);
                }
            },
        };
        if !host.hold(&stream) {
            return;
        }
        let model = self.clone();
        self.spawn(move || model.serve(&host, stream));
    }

    fn stop(&self, served: &Served, stop: Stop) {
        served.end(stop.status());
    }
}
