//! The orchestrator's host and its native Claude, played in the test's own process as the
//! repository's fake orchestrator Claude (`agent-host/orchestrator-fake-claude.py`) plays them behind
//! the real host: each batch's host takes one prompt and answers it as its first marker says, and
//! the native Claude exits once its turn ended, as it does when the host closes its input.
//!
//!   fixture:hold              answers, then holds the turn until the test releases it, after
//!                             telling it `held`
//!   fixture:quiet-hold        holds the turn the same way before it answers anything, telling the
//!                             test its domain capability instead
//!   fixture:tool-then-exit    calls an Alondra tool that changes something, then exits with 1
//!   fixture:read-then-exit    reads, then exits with 1
//!   fixture:denied            calls an Alondra tool and ends with it refused
//!   fixture:denied-first      is refused before calling any tool
//!   fixture:limited           usage runs out before it answers
//!   fixture:exit-before-init  exits with 3 before it initializes
//!   fixture:another-input     ends with the result of a prompt that is not this one
//!
//! A batch that resumes a conversation no earlier batch of the model ended exits before it
//! initializes, as Claude Code does when it finds no conversation to resume. What the fake's wrapper
//! reads from files beside its capture is set on the model: a marker in front of every prompt
//! (`mark`, `mark_once`), a hold before Claude sees its prompt (`hold_before_claude`) and fields
//! every result carries (`add_to_results`).

use super::host_transport::{self, Bootstrap, Doubled, HostState, Identity, lock};
use super::{CAPABILITIES, Trouble};
use crate::protocol::{Action, Command};
use alondra_engine_doubles::agent_host::{Serve, Served};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

const SERVER: &str = "alondra-orchestrator";
const OWN_TOOLS: &[&str] = &["tasks_in_progress", "ask", "create_task", "close_request", "settle_suggestion"];
/// The input tokens each answer reports when the test sets none.
const TOKENS: u64 = 25;
const LIMIT_TEXT: &str = "You've hit your limit";
/// The prompt `fixture:another-input` ends with the result of.
const ANOTHER_INPUT: &str = "00000000-0000-4000-8000-0000000000ff";

/// One orchestrator host the daemon started.
pub type OrchestratorHost = host_transport::Host<Batch>;
type State = HostState<Batch>;

/// The orchestrator's native Claude, shared by the doubled hosts it plays and the test.
#[derive(Clone, Default)]
pub struct OrchestratorModel(Arc<Shared>);

#[derive(Default)]
struct Shared {
    hosts: Mutex<Vec<Arc<OrchestratorHost>>>,
    starting: Arc<AtomicU64>,
    script: Mutex<Script>,
    /// The conversations a batch ended with a result: the only ones a later batch resumes.
    sessions: Mutex<BTreeSet<String>>,
    starts: Mutex<Vec<NativeRun>>,
    prompts: Mutex<Vec<String>>,
    holds: Mutex<Holds>,
    held: Condvar,
    trouble: Mutex<Option<Trouble>>,
    closed: AtomicBool,
}

/// What the test set for the native Claudes that start from now on.
#[derive(Default)]
struct Script {
    marker: Option<String>,
    marker_once: Option<String>,
    hold_before: bool,
    extra: serde_json::Map<String, Value>,
    tokens: Option<u64>,
}

/// The turns held: what each told the test, and the ones that wait for it to release them.
#[derive(Default)]
struct Holds {
    told: VecDeque<String>,
    waiting: VecDeque<(Arc<OrchestratorHost>, Step)>,
}

/// A native Claude a batch's host started: the capability and the command line it got.
#[derive(Clone, Debug)]
struct NativeRun {
    capability: String,
    arguments: Vec<String>,
}

/// Where a turn goes on once the test releases it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Claude takes its prompt.
    Prompt,
    /// Claude answers.
    Answer,
    /// Claude ends its turn with a result.
    Result,
}

/// What one batch's native Claude holds: the prompt it took, as the script it read at its start
/// makes it.
#[derive(Default)]
pub struct Batch {
    serial: u64,
    session: String,
    resumes: bool,
    model: Value,
    cwd: Value,
    input: String,
    prompt: String,
    marker: Option<String>,
    extra: serde_json::Map<String, Value>,
    tokens: u64,
    exited: bool,
}

impl Batch {
    fn uuid(&mut self) -> String {
        self.serial += 1;
        format!("00000000-0000-4000-8000-{:012}", self.serial)
    }
}

impl OrchestratorModel {
    /// Every capability a native Claude started with, one per start.
    pub fn starts(&self) -> Vec<String> {
        lock(&self.0.starts).iter().map(|run| run.capability.clone()).collect()
    }

    /// Every prompt the native Claudes took, in order, with the marker in front of it.
    pub fn prompts(&self) -> Vec<String> {
        lock(&self.0.prompts).clone()
    }

    /// The command line the last native Claude was started with, as far as the model knows it:
    /// its model, its permission mode and the conversation it starts or resumes.
    pub fn arguments(&self) -> Vec<String> {
        lock(&self.0.starts).last().map(|run| run.arguments.clone()).unwrap_or_default()
    }

    /// Every prompt from now on starts with `marker`, or with none.
    pub fn mark(&self, marker: Option<&str>) {
        lock(&self.0.script).marker = marker.map(str::to_owned);
    }

    /// Only the prompts of the next start begin with `marker`.
    pub fn mark_once(&self, marker: &str) {
        lock(&self.0.script).marker_once = Some(marker.to_owned());
    }

    /// The next starts hold their prompt before Claude sees it, until the test releases it.
    pub fn hold_before_claude(&self) {
        lock(&self.0.script).hold_before = true;
    }

    /// Every result from now on carries `fields` too.
    pub fn add_to_results(&self, fields: Value) {
        lock(&self.0.script).extra = fields.as_object().cloned().unwrap_or_default();
    }

    /// Each answer from now on reports `tokens` input tokens: the size of the conversation the test
    /// plays.
    pub fn set_tokens(&self, tokens: u64) {
        lock(&self.0.script).tokens = Some(tokens);
    }

    /// Claude no longer finds any conversation it had: a batch that resumes one exits at once.
    pub fn forget_sessions(&self) {
        lock(&self.0.sessions).clear();
    }

    /// Waits until a native Claude holds its turn, and returns what it told the test: `held`, or
    /// its domain capability for `fixture:quiet-hold`.
    pub fn held(&self) -> String {
        let mut holds = lock(&self.0.holds);
        loop {
            if let Some(told) = holds.told.pop_front() {
                return told;
            }
            holds = self.0.held.wait(holds).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Lets the turn held longest go on, once one is held.
    pub fn release(&self) {
        let (host, step) = {
            let mut holds = lock(&self.0.holds);
            loop {
                if let Some(waiting) = holds.waiting.pop_front() {
                    break waiting;
                }
                holds = self.0.held.wait(holds).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let mut state = lock(&host.state);
        if !state.wire.ended && !state.exited {
            self.play(&host, &mut state, step);
        }
    }

    pub fn trouble(&self) -> Option<Trouble> {
        lock(&self.0.trouble).clone()
    }

    /// Ends every host and starts no other.
    pub fn close(&self) {
        let hosts = {
            let hosts = lock(&self.0.hosts);
            self.0.closed.store(true, Ordering::SeqCst);
            hosts.clone()
        };
        lock(&self.0.holds).waiting.clear();
        for host in hosts {
            host.end(false);
        }
    }

    fn troubled(&self, why: String) {
        lock(&self.0.trouble).get_or_insert(Trouble(why));
    }

    fn host(&self, process_id: u32) -> Option<Arc<OrchestratorHost>> {
        lock(&self.0.hosts).iter().find(|host| host.process_id == process_id).cloned()
    }

    /// A host starts with `bootstrap`, as the doubled process `served`. The model plays only the
    /// orchestrator's hosts.
    fn start(&self, served: &Served) -> io::Result<Arc<OrchestratorHost>> {
        let raw: Value = serde_json::from_slice(served.bootstrap()).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let (bootstrap, secret) = Bootstrap::read(served.bootstrap())?;
        if bootstrap.task().is_some() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the orchestrator model plays no task's host"));
        }
        let profile = &raw["orchestrator"];
        let session = bootstrap.native.session_id.to_lowercase();
        let batch = Batch {
            session: session.clone(),
            resumes: bootstrap.resumes(),
            model: profile["model"].clone(),
            cwd: json!(bootstrap.native.cwd),
            ..Batch::default()
        };
        let identity = Identity {
            task: "the orchestrator".into(),
            account: String::new(),
            capability: bootstrap.native.env.get("ALONDRA_DOMAIN_CAPABILITY").cloned().unwrap_or_default(),
            session,
            resumes: bootstrap.resumes(),
            process_id: served.process_id(),
        };
        let host = Arc::new(OrchestratorHost::new(identity, (bootstrap, secret), CAPABILITIES, Box::new(Doubled(served.clone())), self.0.starting.clone(), batch));
        let mut hosts = lock(&self.0.hosts);
        if self.0.closed.load(Ordering::SeqCst) {
            drop(hosts);
            host.end(false);
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "the model starts no more hosts"));
        }
        hosts.push(host.clone());
        Ok(host)
    }

    /// The host's native Claude starts on the batch's prompt, reading the script as it is now.
    fn open(&self, host: &Arc<OrchestratorHost>, state: &mut State, input: String, prompt: String) {
        let (marker, hold_before) = {
            let mut script = lock(&self.0.script);
            state.extra = script.extra.clone();
            state.tokens = script.tokens.unwrap_or(TOKENS);
            (script.marker.clone().or_else(|| script.marker_once.take()), script.hold_before)
        };
        let mut arguments = vec!["--permission-mode=auto".to_owned()];
        if let Some(model) = state.model.as_str() {
            arguments.push(format!("--model={model}"));
        }
        arguments.push(if state.resumes { format!("--resume={}", state.session) } else { format!("--session-id={}", state.session) });
        lock(&self.0.starts).push(NativeRun { capability: host.capability.clone(), arguments });
        state.input = input.to_lowercase();
        state.prompt = prompt;
        state.marker = marker;
        if hold_before {
            self.hold(host, "held".into(), Step::Prompt);
        } else {
            self.play(host, state, Step::Prompt);
        }
    }

    /// The turn waits for the test, after telling it `told`.
    fn hold(&self, host: &Arc<OrchestratorHost>, told: String, then: Step) {
        let mut holds = lock(&self.0.holds);
        holds.told.push_back(told);
        holds.waiting.push_back((host.clone(), then));
        self.0.held.notify_all();
    }

    /// Plays the batch's turn from `step` on, as the fake orchestrator Claude does.
    fn play(&self, host: &Arc<OrchestratorHost>, state: &mut State, step: Step) {
        let text = match &state.marker {
            Some(marker) => format!("{marker} {}", state.prompt),
            None => state.prompt.clone(),
        };
        let says = |marker: &str| text.contains(marker);
        if step == Step::Prompt {
            lock(&self.0.prompts).push(text.clone());
            if says("fixture:exit-before-init") || (state.resumes && !lock(&self.0.sessions).contains(&state.session)) {
                return self.exit(host, state, 3);
            }
            self.lifecycle(host, state, "queued");
            self.lifecycle(host, state, "started");
            self.init(host, state);
            if says("fixture:limited") {
                let (event, resets_at) = (state.uuid(), alondra_core::now_millis() / 1000 + 3600);
                let session = state.session.clone();
                self.emit(host, state, json!({"type": "rate_limit_event", "session_id": session, "uuid": event,
                    "rate_limit_info": {"status": "rejected", "resetsAt": resets_at, "rateLimitType": "five_hour"}}));
                self.assistant(host, state, json!([{"type": "text", "text": LIMIT_TEXT}]), Some("rate_limit"));
                self.result(host, state, None, json!({"subtype": "error_during_execution", "is_error": true, "result": LIMIT_TEXT, "terminal_reason": "api_error"}));
                return self.exit(host, state, 0);
            }
            let tool_use = json!([{"type": "tool_use", "id": "toolu_fixture", "name": format!("mcp__{SERVER}__create_task"), "input": {"title": "Fixture"}}]);
            if says("fixture:tool-then-exit") {
                self.assistant(host, state, tool_use, None);
                return self.exit(host, state, 1);
            }
            if says("fixture:read-then-exit") {
                self.assistant(host, state, json!([{"type": "text", "text": "Inspected the current state"}]), None);
                return self.exit(host, state, 1);
            }
            if says("fixture:denied-first") {
                let denials = json!([{"tool_name": format!("mcp__{SERVER}__create_task"), "tool_use_id": "toolu_fixture", "tool_input": {}}]);
                self.result(host, state, None, json!({"permission_denials": denials}));
                return self.exit(host, state, 0);
            }
            if says("fixture:denied") {
                self.assistant(host, state, tool_use, None);
                let denials = json!([{"tool_name": format!("mcp__{SERVER}__create_task"), "tool_use_id": "toolu_fixture", "tool_input": {}}]);
                self.result(host, state, None, json!({"permission_denials": denials}));
                return self.exit(host, state, 0);
            }
            if says("fixture:quiet-hold") {
                return self.hold(host, host.capability.clone(), Step::Answer);
            }
        }
        if step != Step::Result {
            self.assistant(host, state, json!([{"type": "text", "text": "Handling the batch"}]), None);
            if says("fixture:hold") {
                return self.hold(host, "held".into(), Step::Result);
            }
        }
        let input = says("fixture:another-input").then_some(ANOTHER_INPUT);
        self.result(host, state, input, json!({}));
        self.exit(host, state, 0);
    }

    fn lifecycle(&self, host: &OrchestratorHost, state: &mut State, lifecycle: &str) {
        let uuid = state.uuid();
        let (input, session) = (state.input.clone(), state.session.clone());
        self.emit(host, state, json!({"type": "command_lifecycle", "command_uuid": input, "state": lifecycle, "session_id": session, "uuid": uuid}));
    }

    fn init(&self, host: &OrchestratorHost, state: &mut State) {
        let uuid = state.uuid();
        let mut tools: Vec<String> = ["Skill", "Read", "Glob", "Grep"].iter().map(|tool| (*tool).to_owned()).collect();
        tools.extend(OWN_TOOLS.iter().map(|tool| format!("mcp__{SERVER}__{tool}")));
        let init = json!({"type": "system", "subtype": "init", "session_id": state.session, "uuid": uuid, "claude_code_version": "2.1.300-fixture",
            "model": state.model, "cwd": state.cwd, "tools": tools, "mcp_servers": [{"name": SERVER, "status": "connected"}],
            "permissionMode": "auto", "slash_commands": [], "output_style": "default", "skills": [], "plugins": [], "apiKeySource": "none"});
        self.emit(host, state, init);
    }

    fn assistant(&self, host: &OrchestratorHost, state: &mut State, content: Value, error: Option<&str>) {
        let uuid = state.uuid();
        let mut message = json!({"type": "assistant", "session_id": state.session, "uuid": uuid, "parent_tool_use_id": null,
            "message": {"id": format!("msg_{}", state.serial), "type": "message", "role": "assistant", "model": state.model, "content": content,
                "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": state.tokens, "output_tokens": 1}},
            "user_message_uuid": state.input, "user_message_uuids": [state.input]});
        if let Some(error) = error {
            message["error"] = json!(error);
        }
        self.emit(host, state, message);
    }

    /// The turn's result, for `input` or the batch's own prompt, with `fields` and then what the
    /// test added to every result; the conversation can be resumed from then on.
    fn result(&self, host: &OrchestratorHost, state: &mut State, input: Option<&str>, fields: Value) {
        let uuid = state.uuid();
        let input = input.map_or_else(|| state.input.clone(), str::to_owned);
        let mut result = json!({"type": "result", "subtype": "success", "is_error": false, "result": "Batch handled", "session_id": state.session, "uuid": uuid,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "terminal_reason": "completed", "user_message_uuid": input, "user_message_uuids": [input]});
        if let (Some(object), Some(fields)) = (result.as_object_mut(), fields.as_object()) {
            object.extend(fields.clone());
            object.extend(state.extra.clone());
        }
        self.emit(host, state, result);
        self.lifecycle(host, state, "completed");
        lock(&self.0.sessions).insert(state.session.clone());
    }

    /// The native Claude exits with `code`: once its turn ended the host closed its input.
    fn exit(&self, host: &OrchestratorHost, state: &mut State, code: i32) {
        state.exited = true;
        self.event(host, state, json!({"type": "process_exited", "exit_code": code, "signal": null}));
    }

    /// Journals an SDK message as the host classifies it and sends it on the lane.
    fn emit(&self, host: &OrchestratorHost, state: &mut State, payload: Value) {
        let kind = match (payload["type"].as_str(), payload["subtype"].as_str()) {
            (Some("result"), _) => "result",
            (Some("assistant" | "user"), _) => "message",
            (Some("system"), Some("init")) => "initialized",
            _ => "status",
        };
        let observation = json!({"type": "sdk", "kind": kind, "native": super::native_ids(&payload), "failures": super::failures(&payload), "payload": payload});
        self.event(host, state, observation);
    }

    fn event(&self, host: &OrchestratorHost, state: &mut State, observation: Value) {
        if let Err(why) = state.wire.event(&host.bootstrap, observation) {
            self.troubled(why);
        }
    }

    fn command(&self, host: &Arc<OrchestratorHost>, state: &mut State, command: Command) {
        let outcome = match &command.action {
            Action::Start { .. } | Action::Interrupt | Action::Close | Action::ForceStop => json!({"status": "accepted", "phase": "applied"}),
            _ => json!({"status": "rejected", "code": "single_turn", "message": "An orchestrator batch is one turn: a later input waits for the next batch"}),
        };
        if let Err(why) = state.wire.receipt(&host.bootstrap, &command, outcome) {
            return self.troubled(why);
        }
        match command.action {
            Action::Start { input_id, prompt, .. } if state.input.is_empty() => self.open(host, state, input_id.to_string(), prompt),
            Action::Close | Action::ForceStop => {
                lock(&self.0.holds).waiting.retain(|(held, _)| !Arc::ptr_eq(held, host));
                if !state.exited {
                    self.exit(host, state, 0);
                }
                state.wire.closing = true;
            }
            _ => {}
        }
    }
}

impl host_transport::Model for OrchestratorModel {
    type Play = Batch;

    fn tick(&self) {}

    fn troubled(&self, why: String) {
        OrchestratorModel::troubled(self, why);
    }

    fn command(&self, host: &OrchestratorHost, state: &mut State, command: Command) {
        let Some(host) = self.host(host.process_id) else { return self.troubled("a command reached a host the model never started".into()) };
        OrchestratorModel::command(self, &host, state, command);
    }
}

impl Serve for OrchestratorModel {
    fn connect(&self, served: &Served, stream: UnixStream) {
        let host = match self.host(served.process_id()) {
            Some(host) => host,
            None => match self.start(served) {
                Ok(host) => {
                    host_transport::ends_with(served, &host);
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
        std::thread::spawn(move || host_transport::serve(&model, &host, stream));
    }
}
