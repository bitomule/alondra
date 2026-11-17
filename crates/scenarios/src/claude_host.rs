//! The Claude of a scenario: every host the daemon starts for a task, and every run of Claude's
//! command line or of the host's probes, goes through this one edge. A host speaks the daemon's
//! wire protocol with the real codec and proofs on the socket the bootstrap names, and says what a
//! Claude session says: the native messages the SDK reports, framed as the host frames them. The
//! world decides what the provider answers: whether each account is signed in, and whether the API
//! fails for everyone. The runner decides when a turn ends and when a message is read.
//!
//! A host outlives the daemon that started it, as a real one does: its process holds the lock of
//! the host's folder, and a daemon that comes back attaches to the same socket and is sent again
//! what it had not acknowledged.

use alondra_agent_protocol::{
    Action, Attach, Attached, Body, Challenge, Command, Fence, Frame, FramedReader, FramedWriter, Hello, PROTOCOL_VERSION, command_digest,
    host_proof, verify_owner_proof,
};
use alondra_agent_runtime::owned_host::{HostLauncher, HostSpawn, ProbeOutput, ProbeRun};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// What the Claude host announces in its hello: every action the protocol carries.
const CAPABILITIES: &[&str] = &[
    "streaming_input", "owner_attach", "commit_ack", "exact_decisions", "interrupt", "force_stop", "set_model", "set_permission_mode",
    "mcp_status", "mcp_reconnect", "mcp_sign_in", "mcp_clear_auth", "plan_supersession",
];

/// The SDK's own list of what it reports, as the host's contract test keeps it.
const SDK_ERRORS: &str = include_str!("../../agent-protocol/fixtures/sdk-errors.json");

/// The errors the SDK ends an assistant message with when the API refused it.
pub fn assistant_errors() -> Vec<String> {
    let catalog: BTreeMap<String, Vec<String>> = serde_json::from_str(SDK_ERRORS).expect("the SDK's error catalog is JSON");
    catalog.get("SDKAssistantMessageError").cloned().unwrap_or_default()
}

/// The model every account of the scenario offers.
pub const MODEL: &str = "demo-model";

/// How the API answers a turn that fails: the error the SDK names and the HTTP status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub error: String,
    pub status: Option<u16>,
}

impl ApiError {
    /// The error an expired sign-in ends every turn with.
    pub fn signed_out() -> Self {
        Self { error: "authentication_failed".into(), status: Some(401) }
    }
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
}

/// What went wrong between the daemon and the hosts, or something a host was asked it does not do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trouble(pub String);

/// The edge itself, shared by the daemon (as its launcher) and the runner.
#[derive(Clone)]
pub struct ClaudeEdge(Arc<Shared>);

struct Shared {
    home: PathBuf,
    world: Mutex<World>,
    hosts: Mutex<Vec<Arc<Host>>>,
    /// Every frame either side wrote: the runner waits for it to stop moving.
    activity: AtomicU64,
    /// Hosts started that no daemon attached to yet.
    starting: Arc<AtomicU64>,
    trouble: Mutex<Option<Trouble>>,
    /// The runs that reached for something the edge does not answer.
    leaks: Arc<Mutex<Vec<String>>>,
    next_uuid: AtomicU64,
    /// The scenario's virtual clock and runtime, which every host reads the time on.
    clock: alondra_core::clock::Clock,
    runtime: Option<tokio::runtime::Handle>,
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
    /// No daemon attached to this host yet, counted in the edge's hosts starting.
    unattached: std::sync::atomic::AtomicBool,
    starting: Arc<AtomicU64>,
    group: i32,
    bootstrap: Bootstrap,
    secret: [u8; 32],
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    lane: Option<FramedWriter<UnixStream>>,
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
}

#[derive(Deserialize)]
struct Bootstrap {
    socket_path: PathBuf,
    host_instance_id: String,
    control_secret: String,
    fence: Fence,
    execution: alondra_agent_protocol::Execution,
    manifest: Manifest,
    native: Native,
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
    env: BTreeMap<String, String>,
    #[serde(default)]
    resume: Option<Value>,
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

impl ClaudeEdge {
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
        }))
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

    /// Every host started for `task`, in the order they started.
    pub fn hosts_of(&self, task: &str) -> Vec<Arc<Host>> {
        lock(&self.0.hosts).iter().filter(|host| host.task == task).cloned().collect()
    }

    /// Each task whose host runs a turn now, with how many inputs that host was offered and has
    /// not taken yet.
    pub fn turns(&self) -> Vec<(String, usize)> {
        lock(&self.0.hosts)
            .iter()
            .filter(|host| host.alive())
            .filter_map(|host| {
                let state = lock(&host.state);
                state.turn.as_ref().map(|_| (host.task.clone(), state.queued.len()))
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
            state.ended || !running(host.group) || state.lane.is_some() && state.acknowledged >= state.cursor
        })
    }

    /// The process of `task`'s host dies, with whatever it started.
    pub fn kill(&self, task: &str) {
        if let Some(host) = self.running(task) {
            host.end();
        }
    }

    /// Ends every host still running: nothing the scenario started outlives it.
    pub fn end_all(&self) {
        for host in lock(&self.0.hosts).iter() {
            host.end();
        }
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

    /// An input reaches the host: it opens a turn when none runs, and waits in Claude's queue when
    /// one does. A turn the API refuses ends at once.
    fn offered(&self, host: &Host, state: &mut HostState, input: String, prompt: String) {
        let queued = self.uuid();
        self.emit(host, state, "status", json!({"type": "command_lifecycle", "state": "queued", "uuid": queued, "command_uuid": input, "session_id": host.session}));
        state.prompts.insert(input.clone(), prompt.clone());
        state.queued.push_back((input, prompt));
        if state.turn.is_none() {
            self.next_turn(host, state);
        }
    }

    /// Claude takes the next input waiting as a turn, and the next while the API refuses each.
    /// It answers each turn it starts at once; the turn ends when the runner says.
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
                    self.emit(host, state, "message", json!({"type": "assistant", "uuid": answer, "parent_tool_use_id": null, "user_message_uuid": input,
                        "user_message_uuids": [input], "message": {"role": "assistant", "content": [{"type": "text", "text": "Working on it"}]}, "session_id": host.session}));
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
                self.emit(host, state, "message", json!({"type": "assistant", "uuid": answer, "parent_tool_use_id": null, "user_message_uuid": last,
                    "user_message_uuids": taken, "message": {"role": "assistant", "content": [{"type": "text", "text": "Done"}]}, "session_id": host.session}));
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
                self.emit(host, state, "message", json!({"type": "assistant", "uuid": answer, "parent_tool_use_id": null, "user_message_uuid": last,
                    "user_message_uuids": taken, "error": error.error, "session_id": host.session}));
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
        let receipt = json!({"protocol_version": PROTOCOL_VERSION, "body": {"type": "receipt", "fence": host.bootstrap.fence, "execution": host.bootstrap.execution,
            "command_id": id, "command_sha256": digest, "outcome": {"status": "accepted", "phase": "applied"}}});
        let receipt: Frame = match serde_json::from_value(receipt) {
            Ok(receipt) => receipt,
            Err(error) => return self.troubled(format!("the host wrote a receipt that is not a frame: {error}")),
        };
        state.receipts.insert(id, receipt.clone());
        send(state, &receipt);
        self.tick();
        match command.action {
            Action::Start { input_id, prompt, .. } | Action::Offer { input_id, prompt } => self.offered(host, state, input_id.to_string(), prompt),
            Action::Interrupt | Action::ForceStop | Action::Close => {
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
        !lock(&self.state).ended && running(self.group)
    }

    /// The host's process group ends, its lock with it, and its socket stops listening.
    pub(crate) fn end(&self) {
        let mut state = lock(&self.state);
        if std::mem::replace(&mut state.ended, true) {
            return;
        }
        self.attached();
        state.lane = None;
        drop(state);
        unsafe { libc::killpg(self.group, libc::SIGKILL) };
        let _ = UnixStream::connect(self.bootstrap.socket_path.with_file_name("b"));
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

/// Serves one daemon's connection to the host: the attach, what it had not acknowledged, then
/// every frame it sends, until it goes.
fn serve(edge: &ClaudeEdge, host: &Arc<Host>, stream: UnixStream) {
    let Ok(clone) = stream.try_clone() else { return };
    let (mut reader, mut writer) = (FramedReader::new(stream), FramedWriter::new(clone));
    match host.handshake(&mut reader, &mut writer) {
        Ok(()) => host.attached(),
        Err(why) if why.is_empty() => return,
        Err(why) => return edge.troubled(format!("the attach of the host of {}: {why}", host.task)),
    }
    {
        let mut state = lock(&host.state);
        let unacknowledged: Vec<Frame> = state.journal.iter().filter(|(cursor, _)| *cursor > state.acknowledged).map(|(_, frame)| frame.clone()).collect();
        state.lane = Some(writer);
        for frame in &unacknowledged {
            send(&mut state, frame);
        }
    }
    edge.tick();
    while let Ok(Some(frame)) = reader.read() {
        edge.tick();
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
                    host.end();
                    return;
                }
            }
            Body::Command(command) => edge.command(host, &mut state, command),
            other => edge.troubled(format!("the daemon sent the host of {} a frame it never sends: {other:?}", host.task)),
        }
    }
    lock(&host.state).lane = None;
    edge.tick();
}

/// Whether the process `pid` runs: a zombie nobody reaped yet has ended.
fn running(pid: i32) -> bool {
    const ZOMBIE: u32 = 5;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into the record it is given.
    let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: a read of the whole record initialized it.
    read == size && unsafe { info.assume_init() }.pbi_status != ZOMBIE
}

/// The relay a host's process runs, which Bazel builds beside the scenarios.
fn relay() -> Result<PathBuf, String> {
    let path = std::env::var_os("SCENARIO_HOST_RELAY").ok_or("SCENARIO_HOST_RELAY names no relay: run the scenarios through Bazel")?;
    std::fs::canonicalize(&path).map_err(|error| format!("the host relay {}: {error}", PathBuf::from(path).display()))
}

/// Which of the scenario's accounts a run of Claude is for: the account folder its configuration
/// names, or the default account's own when it names none.
fn account_of(home: &Path, environment: &BTreeMap<String, String>) -> String {
    let folders = alondra_core::accounts::folders(home);
    environment
        .get("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|folder| folder.parent() == Some(folders.as_path()))
        .and_then(|folder| folder.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| alondra_core::accounts::AccountId::default_account().as_str().to_owned())
}

impl HostLauncher for ClaudeEdge {
    fn spawn(&self, mut spawn: HostSpawn<'_>) -> std::io::Result<Child> {
        let invalid = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidData, why);
        let mut text = String::new();
        spawn.stdin.read_to_string(&mut text)?;
        let bootstrap: Bootstrap = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
        let secret = decode_secret(&bootstrap.control_secret).ok_or_else(|| invalid("the control secret is not 32 bytes of hex".into()))?;
        let alondra_agent_protocol::Execution::Task(execution) = &bootstrap.execution else {
            return Err(invalid("the scenario runs no orchestrator host".into()));
        };
        let task = execution.task_id.to_string();
        if lock(&self.0.world).unstartable.contains(&task) {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "the host's executable cannot start"));
        }
        let back = bootstrap.socket_path.with_file_name("b");
        let listener = UnixListener::bind(&back)?;
        std::fs::set_permissions(&back, std::fs::Permissions::from_mode(0o600))?;
        // The process the daemon supervises listens on the host's socket and holds the lock of the
        // host's folder, which the daemon hands over as its input, for as long as it lives: a
        // daemon that comes back finds it held, and finds it at the other end of the socket.
        let relay = relay().map_err(std::io::Error::other)?;
        let child = Process::new(relay)
            .arg(&bootstrap.socket_path)
            .arg(&back)
            .stdin(Stdio::from(spawn.stdin))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        self.0.starting.fetch_add(1, Ordering::SeqCst);
        let host = Arc::new(Host {
            account: account_of(&self.0.home, &bootstrap.native.env),
            capability: bootstrap.native.env.get("ALONDRA_DOMAIN_CAPABILITY").cloned().unwrap_or_default(),
            session: bootstrap.native.session_id.clone(),
            resumes: bootstrap.native.resume.as_ref().is_some_and(|resume| !resume.is_null() && *resume != Value::Bool(false)),
            group: child.id() as i32,
            unattached: std::sync::atomic::AtomicBool::new(true),
            starting: self.0.starting.clone(),
            task,
            secret,
            state: Mutex::new(HostState { cursor: bootstrap.journal_base, acknowledged: bootstrap.journal_base, ..HostState::default() }),
            bootstrap,
        });
        lock(&self.0.hosts).push(host.clone());
        self.tick();
        let edge = self.clone();
        std::thread::spawn(move || {
            let _entered = alondra_core::clock::enter(edge.0.clock, edge.0.runtime.as_ref());
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                if lock(&host.state).ended {
                    return;
                }
                serve(&edge, &host, stream);
                if lock(&host.state).ended {
                    return;
                }
            }
        });
        Ok(child)
    }

    fn run(&self, run: ProbeRun) -> std::io::Result<ProbeOutput> {
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
        Err(std::io::Error::other(format!("a scenario runs no {what}")))
    }
}

#[cfg(test)]
#[path = "claude_host_tests.rs"]
mod tests;
