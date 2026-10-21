//! The native Claude Code the Agent SDK host spawns (`claude --output-format stream-json --verbose
//! --input-format stream-json …`), scripted. It speaks the host's stream-json protocol, as
//! `agent-host/persistent-fake-claude.py` does, and follows its task's script: every input opens or
//! joins a turn, which ends once the session delivers, asks or cleans up. A journey moves a session
//! through `fakes/sessions/<short>/order.sock`, so each step runs inside the session's own turn.
//! The session keeps a transcript, so `--resume` finds its conversation, and writes down each start.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Fakes, append, mcp, node::VERSION, orchestrator, read_lines,
    script::{Step, TaskScript},
    work, write_json,
};
use crate::sandbox::{hex, now_ms, random};

/// The arguments the Agent SDK starts the native executable with, before its options.
const SPAWN: [&str; 5] = ["--output-format", "stream-json", "--verbose", "--input-format", "stream-json"];

pub fn spawned_by_sdk(args: &[String]) -> bool {
    args.len() >= SPAWN.len() && args.iter().zip(SPAWN).all(|(arg, expected)| arg == expected)
}

/// An orchestrator batch (`--restricted`) or a task's session.
pub fn run(fakes: &Fakes, args: &[String]) -> Result<i32> {
    if args.iter().any(|arg| arg == "--restricted") {
        return orchestrator::batch(fakes, args);
    }
    worker(fakes, args)
}

/// The value of `name` in either form the SDK writes it: `--name=value` or `--name value`.
pub fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(at, arg)| match arg.strip_prefix(name) {
        Some(rest) if rest.starts_with('=') => Some(&rest[1..]),
        Some("") => args.get(at + 1).map(String::as_str),
        _ => None,
    })
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    /// Its input closed, or it exited on its own: the host stopped it or the session ended.
    Ended,
    /// Killed without a word, as a crashed Claude.
    Dead,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub session_id: String,
    pub short: String,
    pub task_id: Option<String>,
    pub cwd: PathBuf,
    /// The Alondra environment the host gave its latest start.
    pub env: Vec<(String, String)>,
    pub pid: Option<u32>,
    pub state: State,
    pub starts: u32,
    pub started_at: u64,
}

/// One start of the session's process, as the journeys read it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Start {
    pub at: u64,
    pub pid: u32,
    pub resume: Option<String>,
    /// How many inputs its transcript held when it started.
    pub history: usize,
    pub cwd: PathBuf,
}

/// One input the session took.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Received {
    pub at: u64,
    pub uuid: String,
    pub text: String,
}

/// What a journey asks the session to do: steps, in its current turn, which ends afterwards when
/// `end_turn` says so or a step left a card.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Order {
    pub steps: Vec<Step>,
    #[serde(default)]
    pub end_turn: bool,
}

pub use super::node::Outcome;

impl Fakes {
    fn sessions(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    /// Short enough for the order socket's path in any sandbox.
    fn session_dir(&self, short: &str) -> PathBuf {
        self.sessions().join(short)
    }

    pub fn session(&self, short: &str) -> Result<Session> {
        let path = self.session_dir(short).join("session.json");
        Ok(serde_json::from_slice(&fs::read(&path).with_context(|| format!("no native session {short}"))?)?)
    }

    fn save_session(&self, session: &Session) -> Result<()> {
        write_json(&self.session_dir(&session.short).join("session.json"), session)
    }

    pub fn all_sessions(&self) -> Result<Vec<Session>> {
        let mut sessions = vec![];
        let Ok(entries) = fs::read_dir(self.sessions()) else { return Ok(sessions) };
        for entry in entries {
            if let Ok(session) = self.session(&entry?.file_name().to_string_lossy()) {
                sessions.push(session);
            }
        }
        sessions.sort_by_key(|session| session.started_at);
        Ok(sessions)
    }

    /// The session of task `task_id` that started last.
    pub fn session_of(&self, task_id: &str) -> Result<Option<Session>> {
        Ok(self.all_sessions()?.into_iter().filter(|session| session.task_id.as_deref() == Some(task_id)).next_back())
    }

    pub fn received(&self, short: &str) -> Result<Vec<Received>> {
        read_lines(&self.session_dir(short).join("received.jsonl"))
    }

    pub fn session_outcomes(&self, short: &str) -> Result<Vec<Outcome>> {
        read_lines(&self.session_dir(short).join("steps.jsonl"))
    }

    pub fn starts(&self, short: &str) -> Result<Vec<Start>> {
        read_lines(&self.session_dir(short).join("starts.jsonl"))
    }

    /// The files a journey's report keeps of each session.
    pub fn session_files(&self) -> Result<Vec<(String, PathBuf)>> {
        Ok(self
            .all_sessions()?
            .into_iter()
            .flat_map(|session| {
                let dir = self.session_dir(&session.short);
                ["session.json", "starts.jsonl", "received.jsonl", "steps.jsonl", "native.log"]
                    .into_iter()
                    .map(move |name| (format!("session-{}-{name}", session.short), dir.join(name)))
            })
            .filter(|(_, path)| path.exists())
            .collect())
    }
}

impl Session {
    /// Whether its process still runs.
    pub fn alive(&self) -> bool {
        self.pid.is_some_and(|pid| Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|status| status.success()))
    }

    pub fn running(&self) -> bool {
        self.state == State::Running && self.alive()
    }

    pub fn capability(&self) -> Option<String> {
        self.env.iter().find(|(key, _)| key == "ALONDRA_DOMAIN_CAPABILITY").map(|(_, value)| value.clone())
    }
}

/// Calls a node tool with only `capability`, as a session still holding an old bearer would:
/// whether it was refused, and its text.
pub fn call_with(fakes: &Fakes, session: &Session, capability: &str, name: &str, arguments: Value) -> Result<(bool, String)> {
    let home = session.env.iter().find(|(key, _)| key == "ALONDRA_HOME").map(|(_, value)| value.clone()).context("the session has no ALONDRA_HOME")?;
    let env = vec![
        ("ALONDRA_HOME".to_owned(), home),
        ("ALONDRA_DOMAIN_PROTOCOL".to_owned(), "2".to_owned()),
        ("ALONDRA_DOMAIN_CAPABILITY".to_owned(), capability.to_owned()),
    ];
    let reply = mcp::call(&fakes.alondra(), &["mcp-node".into()], &env, &[(name, arguments)])?.pop().context("no reply")?;
    Ok((reply.refused, reply.text))
}

/// Has the running session do `order`; what each step did. A session that ends or dies on a step
/// answers up to it.
pub fn order(fakes: &Fakes, session: &Session, order: &Order) -> Result<Vec<Outcome>> {
    let socket = fakes.session_dir(&session.short).join("order.sock");
    let mut stream = UnixStream::connect(&socket).with_context(|| format!("session {} takes no orders at {}", session.short, socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(180)))?;
    writeln!(stream, "{}", serde_json::to_string(order)?)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).with_context(|| format!("session {} did not answer its order", session.short))?;
    if line.trim().is_empty() {
        bail!("session {} ended without answering its order", session.short);
    }
    Ok(serde_json::from_str(&line)?)
}

/// The Alondra environment of this process, as the host set it.
fn alondra_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(key, _)| key.starts_with("ALONDRA_")).collect();
    env.sort();
    env
}

pub fn uuid() -> Result<String> {
    let text = hex(&random::<16>()?);
    Ok(format!("{}-{}-4{}-8{}-{}", &text[0..8], &text[8..12], &text[13..16], &text[17..20], &text[20..32]))
}

/// The stream the host reads, one JSON message per line.
#[derive(Clone)]
struct Out(Arc<Mutex<std::io::Stdout>>);

impl Out {
    fn send(&self, value: &Value) -> Result<()> {
        let mut out = self.0.lock().map_err(|_| anyhow!("stdout poisoned"))?;
        writeln!(out, "{value}")?;
        out.flush()?;
        Ok(())
    }

    fn control(&self, request: &str, response: Value) -> Result<()> {
        self.send(&json!({"type": "control_response", "response": {"subtype": "success", "request_id": request, "response": response}}))
    }
}

enum Event {
    Input { uuid: String, text: String },
    Order { order: Order, reply: mpsc::Sender<Vec<Outcome>>, written: mpsc::Receiver<()> },
    Interrupt,
    Closed,
}

/// The text of a user message: a string, or its text blocks.
fn text_of(message: &Value) -> String {
    match &message["message"]["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Answers the host's control requests at once and hands inputs to the session, until the host
/// closes its input.
/// The permission requests the session waits on, by request id: each gets the host's answer.
type Asked = Arc<Mutex<std::collections::BTreeMap<String, mpsc::Sender<Value>>>>;

fn read_input(out: &Out, events: &mpsc::Sender<Event>, asked: &Asked, servers: &Arc<super::oauth::Servers>, log: &Path) -> Result<()> {
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line).with_context(|| format!("not JSON: {line}"))?;
        match message["type"].as_str() {
            Some("control_request") => {
                let request = message["request_id"].as_str().unwrap_or_default();
                match message["request"]["subtype"].as_str() {
                    Some("initialize") => out.control(request, json!({
                        "commands": [], "account": {}, "output_style": "default",
                        "models": (["sonnet", "opus", "haiku"].map(|model| json!({"value": model, "displayName": model, "description": "A sandbox model"}))),
                    }))?,
                    Some("interrupt") => {
                        out.control(request, json!({"still_queued": []}))?;
                        let _ = events.send(Event::Interrupt);
                    }
                    Some("set_model" | "set_permission_mode") => out.control(request, json!({}))?,
                    Some("mcp_status") => out.control(request, json!({"mcpServers": servers.statuses()}))?,
                    Some(subtype @ ("mcp_reconnect" | "mcp_authenticate")) => {
                        let name = message["request"]["serverName"].as_str().unwrap_or_default();
                        let answer = if subtype == "mcp_reconnect" { servers.reconnect(name).map(|()| json!({})) } else { servers.authenticate(name) };
                        note(log, &format!("{subtype} {name}: {}", if answer.is_ok() { "ok" } else { "refused" }));
                        match answer {
                            Ok(response) => out.control(request, response)?,
                            Err(error) => out.send(&json!({"type": "control_response", "response": {"subtype": "error", "request_id": request, "error": error}}))?,
                        }
                    }
                    other => {
                        note(log, &format!("refused control request {other:?}"));
                        out.send(&json!({"type": "control_response", "response": {"subtype": "error", "request_id": request, "error": format!("the sandbox's Claude does not take {other:?}")}}))?;
                    }
                }
            }
            Some("user") => {
                let uuid = message["uuid"].as_str().context("an input without a uuid")?.to_lowercase();
                let _ = events.send(Event::Input { uuid, text: text_of(&message) });
            }
            Some(kind @ ("control_response" | "control_cancel_request")) => {
                let request = if kind == "control_response" { &message["response"]["request_id"] } else { &message["request_id"] };
                let waiting = request.as_str().and_then(|request| asked.lock().ok()?.remove(request));
                match waiting {
                    Some(answer) => {
                        let _ = answer.send(if kind == "control_response" { message["response"].clone() } else { json!({"cancelled": true}) });
                    }
                    None => note(log, &format!("a {kind} for no request it waits on: {line}")),
                }
            }
            other => note(log, &format!("ignored {other:?}")),
        }
    }
    let _ = events.send(Event::Closed);
    Ok(())
}

/// Takes one order per connection to the session's socket and answers with what it did.
fn serve_orders(listener: UnixListener, events: mpsc::Sender<Event>, log: PathBuf) {
    for stream in listener.incoming() {
        let served = (|| -> Result<()> {
            let mut stream = stream?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            let order: Order = serde_json::from_str(&line).context("an order is one JSON line")?;
            let (reply, answers) = mpsc::channel();
            let (wrote, written) = mpsc::channel();
            events.send(Event::Order { order, reply, written }).map_err(|_| anyhow!("the session ended"))?;
            let outcomes = answers.recv().map_err(|_| anyhow!("the session ended before it answered"))?;
            writeln!(stream, "{}", serde_json::to_string(&outcomes)?)?;
            stream.flush()?;
            let _ = wrote.send(());
            Ok(())
        })();
        if let Err(error) = served {
            note(&log, &format!("order: {error:#}"));
        }
    }
}

fn note(log: &Path, text: &str) {
    let line = format!("{} {text}\n", now_ms());
    let _ = fs::OpenOptions::new().create(true).append(true).open(log).and_then(|mut file| file.write_all(line.as_bytes()));
}

fn worker(fakes: &Fakes, args: &[String]) -> Result<i32> {
    let resume = flag(args, "--resume").map(str::to_lowercase);
    let session_id = resume.clone().or_else(|| flag(args, "--session-id").map(str::to_lowercase)).context("the SDK named no session")?;
    let short = session_id.get(..8).context("a session id is a UUID")?.to_owned();
    let dir = fakes.session_dir(&short);
    fs::create_dir_all(&dir)?;
    let transcript = dir.join("transcript.jsonl");
    let history = read_lines::<Value>(&transcript)?.len();
    if resume.is_some() && history == 0 {
        eprintln!("No conversation found with session ID: {session_id}");
        return Ok(1);
    }
    let earlier = fakes.session(&short).ok();
    let cwd = std::env::current_dir()?;
    let socket = dir.join("order.sock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    let session = Session {
        session_id: session_id.clone(),
        short: short.clone(),
        task_id: earlier.as_ref().and_then(|earlier| earlier.task_id.clone()),
        cwd: cwd.clone(),
        env: alondra_env(),
        pid: Some(std::process::id()),
        state: State::Running,
        starts: earlier.as_ref().map_or(0, |earlier| earlier.starts) + 1,
        started_at: now_ms(),
    };
    fakes.save_session(&session)?;
    let start = Start { at: now_ms(), pid: std::process::id(), resume, history, cwd };
    append(&dir.join("starts.jsonl"), &serde_json::to_value(&start)?)?;
    let log = dir.join("native.log");
    note(&log, &format!("started with {}", args.join(" ")));
    let out = Out(Arc::new(Mutex::new(std::io::stdout())));
    let (events, inbox) = mpsc::channel();
    let asked = Asked::default();
    let servers = super::oauth::Servers::load(fakes);
    {
        let (out, events, asked, servers, log) = (out.clone(), events.clone(), asked.clone(), servers.clone(), log.clone());
        std::thread::spawn(move || {
            if let Err(error) = read_input(&out, &events, &asked, &servers, &log) {
                note(&log, &format!("input: {error:#}"));
                let _ = events.send(Event::Closed);
            }
            if let Ok(mut waiting) = asked.lock() {
                waiting.clear();
            }
        });
    }
    {
        let log = log.clone();
        std::thread::spawn(move || serve_orders(listener, events, log));
    }
    let script = match &session.task_id {
        Some(task) => fakes.task_script(task)?,
        None => None,
    };
    let mut native = Native { fakes, out, session, script, turn: None, own: None, fresh: history == 0, asked, servers, dir, log };
    let ended = native.live(inbox);
    let _ = fs::remove_file(&socket);
    if native.session.state == State::Running {
        native.session.state = State::Ended;
        fakes.save_session(&native.session)?;
    }
    if let Err(error) = &ended {
        note(&native.log, &format!("ended: {error:#}"));
    }
    ended.map(|()| 0)
}

/// While this file exists in `fakes/`, the native Claude accepts an interrupt and its turn goes on,
/// as one that only a force stop ends.
pub const HOLD_INTERRUPT: &str = "hold-interrupt";
/// While this file exists in `fakes/`, each turn ends as Claude Code ends one at its turn limit.
pub const FAIL_TURN: &str = "fail-turn";
/// While this file exists in `fakes/`, the native Claude completes each input after the mission
/// without starting it, as one its transcript already holds: no turn ever answers it.
pub const DROP_INPUT: &str = "drop-input";

struct Native<'a> {
    fakes: &'a Fakes,
    out: Out,
    session: Session,
    script: Option<TaskScript>,
    /// The inputs of the open turn, the latest last.
    turn: Option<Vec<String>>,
    /// The inputs that wait queued behind a turn Claude started by itself, while it runs.
    own: Option<Vec<String>>,
    /// Its next input is the mission of a session that never ran before.
    fresh: bool,
    asked: Asked,
    servers: Arc<super::oauth::Servers>,
    dir: PathBuf,
    log: PathBuf,
}

/// How the session leaves after an order.
enum Leave {
    Stay,
    Exit,
    Die,
}

impl Native<'_> {
    fn live(&mut self, inbox: mpsc::Receiver<Event>) -> Result<()> {
        for event in inbox {
            match event {
                Event::Input { uuid, text } => self.input(&uuid, &text)?,
                Event::Interrupt => self.interrupted()?,
                Event::Order { order, reply, written } => {
                    let (outcomes, leave) = self.order(&order)?;
                    let _ = reply.send(outcomes);
                    match leave {
                        Leave::Stay => {}
                        Leave::Exit => {
                            let _ = written.recv_timeout(Duration::from_secs(5));
                            return Ok(());
                        }
                        Leave::Die => {
                            let _ = written.recv_timeout(Duration::from_secs(5));
                            self.die()?;
                        }
                    }
                }
                Event::Closed => return Ok(()),
            }
        }
        Ok(())
    }

    fn send(&self, value: Value) -> Result<()> {
        self.out.send(&value)
    }

    fn lifecycle(&self, input: &str, state: &str) -> Result<()> {
        self.send(json!({"type": "command_lifecycle", "command_uuid": input, "state": state, "session_id": self.session.session_id, "uuid": uuid()?}))
    }

    /// The inputs a message of the open turn answers: all of them, the latest as its own.
    fn answering(&self) -> (Value, Value) {
        let inputs = self.turn.clone().unwrap_or_default();
        (json!(inputs.last()), json!(inputs))
    }

    fn assistant(&self, content: Value) -> Result<()> {
        let (input, inputs) = self.answering();
        let id = uuid()?;
        self.send(json!({
            "type": "assistant", "session_id": self.session.session_id, "uuid": id, "parent_tool_use_id": null,
            "message": {"id": format!("msg_{}", hex(&random::<8>()?)), "type": "message", "role": "assistant", "model": "sandbox",
                "content": content, "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}},
            "user_message_uuid": input, "user_message_uuids": inputs,
        }))
    }

    fn input(&mut self, input: &str, text: &str) -> Result<()> {
        let received = Received { at: now_ms(), uuid: input.into(), text: text.into() };
        append(&self.dir.join("transcript.jsonl"), &json!(text))?;
        append(&self.dir.join("received.jsonl"), &serde_json::to_value(&received)?)?;
        let mission = std::mem::take(&mut self.fresh);
        if mission && self.session.task_id.is_none() {
            let named = self.fakes.task_script_in(text)?.map(|task| task.task_id);
            self.session.task_id = named.or_else(|| self.fakes.task_script(&self.session.cwd.file_name()?.to_string_lossy()).ok().flatten().map(|task| task.task_id));
            self.fakes.save_session(&self.session)?;
            self.script = match &self.session.task_id {
                Some(task) => self.fakes.task_script(task)?,
                None => None,
            };
        }
        self.lifecycle(input, "queued")?;
        if let Some(queued) = self.own.as_mut() {
            queued.push(input.into());
            return Ok(());
        }
        if !mission && self.fakes.dir.join(DROP_INPUT).exists() {
            return self.lifecycle(input, "completed");
        }
        self.lifecycle(input, "started")?;
        if self.turn.is_none() {
            self.send(json!({
                "type": "system", "subtype": "init", "session_id": self.session.session_id, "uuid": uuid()?,
                "claude_code_version": VERSION, "model": "sandbox", "cwd": self.session.cwd,
                "tools": ["Bash", "Read", "Write", "mcp__plugin_alondra_alondra__present_result", "mcp__plugin_alondra_alondra__ask"],
                "mcp_servers": self.servers.statuses().iter().map(|server| json!({"name": server["name"], "status": server["status"]})).collect::<Vec<_>>(),
                "permissionMode": "default", "slash_commands": [], "output_style": "default", "skills": ["alondra:alondra"],
                "plugins": [], "apiKeySource": "none",
            }))?;
            self.turn = Some(vec![]);
        }
        self.turn.get_or_insert_with(Vec::new).push(input.into());
        self.assistant(json!([{"type": "text", "text": "On it."}]))?;
        let Some(script) = self.script.clone() else { return Ok(()) };
        let steps = if mission {
            script.on_start.clone()
        } else if text.contains("cleanup_done") || text.contains("is finished and is closing") {
            if script.cleans_up { vec![Step::Cleanup] } else { vec![] }
        } else if is_feedback(text) {
            script.on_feedback.clone()
        } else {
            vec![]
        };
        if !steps.is_empty() {
            self.order(&Order { steps, end_turn: true })?;
        }
        Ok(())
    }

    fn interrupted(&mut self) -> Result<()> {
        if self.fakes.dir.join(HOLD_INTERRUPT).exists() {
            return Ok(());
        }
        let Some(inputs) = self.turn.take() else { return Ok(()) };
        let last = inputs.last().cloned().unwrap_or_default();
        self.send(json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true, "session_id": self.session.session_id, "uuid": uuid()?,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 0, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "errors": ["Interrupted by the user"], "user_message_uuid": last,
        }))?;
        for input in &inputs {
            self.lifecycle(input, "completed")?;
        }
        Ok(())
    }

    /// The open turn ends with `said`.
    fn end_turn(&mut self, said: &str) -> Result<()> {
        let (input, inputs) = self.answering();
        let Some(open) = self.turn.take() else { return Ok(()) };
        let mut result = json!({
            "type": "result", "subtype": "success", "is_error": false, "session_id": self.session.session_id, "uuid": uuid()?,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "result": said, "terminal_reason": "completed", "user_message_uuid": input, "user_message_uuids": inputs,
        });
        if self.fakes.dir.join(FAIL_TURN).exists() {
            result["subtype"] = json!("error_max_turns");
            result["is_error"] = json!(true);
            result["terminal_reason"] = json!("max_turns");
            result["errors"] = json!(["Reached maximum number of turns (1)"]);
        }
        self.send(result)?;
        for input in &open {
            self.lifecycle(input, "completed")?;
        }
        Ok(())
    }

    fn order(&mut self, order: &Order) -> Result<(Vec<Outcome>, Leave)> {
        let mut outcomes = vec![];
        let mut ends = order.end_turn;
        for step in &order.steps {
            let (outcome, leave) = self.act(step)?;
            ends |= outcome.ok && step.leaves_card();
            outcomes.push(outcome);
            if !matches!(leave, Leave::Stay) {
                return Ok((outcomes, leave));
            }
        }
        if ends {
            self.end_turn("Done.")?;
        }
        Ok((outcomes, Leave::Stay))
    }

    fn act(&mut self, step: &Step) -> Result<(Outcome, Leave)> {
        let (reply, leave) = match step {
            Step::Speak { said, behind, .. } => {
                if self.turn.is_some() {
                    self.assistant(json!([{"type": "text", "text": said}]))?;
                    if let Some(command) = behind {
                        let id = format!("toolu_{}", hex(&random::<6>()?));
                        self.assistant(json!([{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command, "run_in_background": true}}]))?;
                        self.send(json!({
                            "type": "system", "subtype": "task_started", "task_id": format!("bg-{}", hex(&random::<4>()?)), "tool_use_id": id,
                            "description": command, "task_type": "local_bash", "is_backgrounded": true, "uuid": uuid()?, "session_id": self.session.session_id,
                        }))?;
                    }
                    self.end_turn(said)?;
                }
                (Ok(work::done("said")), Leave::Stay)
            }
            Step::OwnTurn { said } => (self.own_turn(said), Leave::Stay),
            Step::LeavePlanMode { points } => (self.leave_plan_mode(*points), Leave::Stay),
            Step::EndOwnTurn => (self.end_own_turn(), Leave::Stay),
            Step::End => (Ok(work::done("ended")), Leave::Exit),
            Step::Die => (Ok(work::done("killed")), Leave::Die),
            _ => {
                let tool = work::tool_of(step);
                let id = format!("toolu_{}", hex(&random::<6>()?));
                if let (Some((name, input)), Some(_)) = (&tool, &self.turn) {
                    self.assistant(json!([{"type": "tool_use", "id": id, "name": name, "input": input}]))?;
                }
                let denied = match step {
                    Step::Bash { command } => self.permission(&id, command)?,
                    _ => None,
                };
                let env = || Ok(alondra_env());
                let worker = work::Worker { task: self.session.task_id.as_deref(), dir: &self.session.cwd, short: &self.session.short, env: &env };
                let reply = match denied {
                    Some(why) => Ok(mcp::Reply { refused: true, text: why }),
                    None => work::perform(self.fakes, &worker, step).unwrap_or_else(|| Err(anyhow!("no step"))),
                };
                if tool.is_some() && self.turn.is_some() {
                    let (refused, text) = match &reply {
                        Ok(reply) => (reply.refused, reply.text.clone()),
                        Err(error) => (true, format!("{error:#}")),
                    };
                    self.send(json!({
                        "type": "user", "session_id": self.session.session_id, "uuid": uuid()?, "parent_tool_use_id": null,
                        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "is_error": refused, "content": text}]},
                    }))?;
                }
                (reply, Leave::Stay)
            }
        };
        let (ok, text) = match reply {
            Ok(reply) => (!reply.refused, reply.text),
            Err(error) => (false, format!("{error:#}")),
        };
        let outcome = Outcome { at: now_ms(), step: step.clone(), ok, text };
        append(&self.dir.join("steps.jsonl"), &serde_json::to_value(&outcome)?)?;
        if matches!(leave, Leave::Die) {
            self.session.state = State::Dead;
            self.fakes.save_session(&self.session)?;
        }
        Ok((outcome, leave))
    }

    /// Asks the host whether `command` may run, as Claude Code does in default mode for a command
    /// its permission rules do not allow, and waits for the answer: why it may not, or `None`.
    fn permission(&mut self, tool_use: &str, command: &str) -> Result<Option<String>> {
        if allowed_by_rules(&self.session.cwd, command) {
            append(&self.dir.join("permissions.jsonl"), &json!({"at": now_ms(), "command": command, "asked": false}))?;
            return Ok(None);
        }
        let request = format!("permission-{}", hex(&random::<6>()?));
        let (answer, answered) = mpsc::channel();
        self.asked.lock().map_err(|_| anyhow!("asked poisoned"))?.insert(request.clone(), answer);
        self.send(json!({
            "type": "control_request", "request_id": request,
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": command}, "tool_use_id": tool_use,
                "permission_suggestions": suggestions(command)},
        }))?;
        let reply = answered.recv().unwrap_or_else(|_| json!({"cancelled": true}));
        let decision = &reply["response"];
        let allowed = decision["behavior"] == "allow";
        append(&self.dir.join("permissions.jsonl"), &json!({"at": now_ms(), "command": command, "asked": true, "answer": reply}))?;
        if allowed {
            return Ok(None);
        }
        Ok(Some(decision["message"].as_str().unwrap_or("The user did not allow the command").to_owned()))
    }

    /// A root message of the turn Claude started by itself: it answers no input of Alondra's.
    fn own_message(&self, content: Value) -> Result<()> {
        self.send(json!({
            "type": "assistant", "session_id": self.session.session_id, "uuid": uuid()?, "parent_tool_use_id": null,
            "message": {"id": format!("msg_{}", hex(&random::<8>()?)), "type": "message", "role": "assistant", "model": "sandbox",
                "content": content, "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}},
        }))
    }

    fn own_turn(&mut self, said: &str) -> Result<mcp::Reply> {
        if self.turn.is_some() || self.own.is_some() {
            bail!("a turn is already open");
        }
        self.send(json!({
            "type": "system", "subtype": "init", "session_id": self.session.session_id, "uuid": uuid()?,
            "claude_code_version": VERSION, "model": "sandbox", "cwd": self.session.cwd, "tools": ["Bash", "ExitPlanMode"],
            "mcp_servers": [], "permissionMode": "plan", "slash_commands": [], "output_style": "default", "skills": [], "plugins": [], "apiKeySource": "none",
        }))?;
        self.own = Some(vec![]);
        self.own_message(json!([{"type": "text", "text": said}]))?;
        Ok(work::done("started a turn of its own"))
    }

    /// Asks the host to leave plan mode with a plan of `points` points, as Claude Code does with
    /// the plan file Claude wrote, and waits: what Claude was told.
    fn leave_plan_mode(&mut self, points: usize) -> Result<mcp::Reply> {
        if self.own.is_none() {
            bail!("no turn of its own is open");
        }
        let plan = format!("# Sandbox plan\n\n{}", (1..=points).map(|point| format!("- [ ] Point {point}")).collect::<Vec<_>>().join("\n"));
        let file = self.dir.join("plan.md");
        fs::write(&file, &plan)?;
        let tool_use = format!("toolu_{}", hex(&random::<6>()?));
        self.own_message(json!([{"type": "tool_use", "id": tool_use, "name": "ExitPlanMode", "input": {}}]))?;
        let request = format!("plan-{}", hex(&random::<6>()?));
        let (answer, answered) = mpsc::channel();
        self.asked.lock().map_err(|_| anyhow!("asked poisoned"))?.insert(request.clone(), answer);
        self.send(json!({
            "type": "control_request", "request_id": request,
            "request": {"subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": {"plan": plan, "planFilePath": file}, "tool_use_id": tool_use},
        }))?;
        let reply = answered.recv().unwrap_or_else(|_| json!({"cancelled": true}));
        let decision = &reply["response"];
        let allowed = decision["behavior"] == "allow";
        let told = if allowed { "User has approved your plan".to_owned() } else { decision["message"].as_str().unwrap_or("denied").to_owned() };
        self.send(json!({
            "type": "user", "session_id": self.session.session_id, "uuid": uuid()?, "parent_tool_use_id": null,
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_use, "is_error": !allowed, "content": told}]},
        }))?;
        Ok(mcp::Reply { refused: !allowed, text: told })
    }

    fn end_own_turn(&mut self) -> Result<mcp::Reply> {
        let queued = self.own.take().context("no turn of its own is open")?;
        self.send(json!({
            "type": "result", "subtype": "success", "is_error": false, "session_id": self.session.session_id, "uuid": uuid()?,
            "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": [],
            "result": "Done.", "terminal_reason": "completed", "user_message_uuid": uuid()?,
        }))?;
        if !queued.is_empty() {
            for input in &queued {
                self.lifecycle(input, "started")?;
            }
            self.turn = Some(queued);
            self.assistant(json!([{"type": "text", "text": "On it."}]))?;
            self.end_turn("Done.")?;
        }
        Ok(work::done("ended its own turn"))
    }

    /// Dies at once, without a word, as a crashed Claude does; its SDK host goes on.
    fn die(&self) -> Result<()> {
        note(&self.log, "killed");
        Command::new("kill").args(["-KILL", &std::process::id().to_string()]).status()?;
        bail!("the session outlived SIGKILL")
    }
}

/// Alondra hands a result back (changes, a question or reply about it, a failed merge) with the
/// instruction to present it again.
pub fn is_feedback(text: &str) -> bool {
    text.contains("present the result again") || text.contains("present_result again")
}

/// Programs Claude Code runs without asking, as the sandbox's commands use them.
const READ_ONLY: [&str; 5] = ["tail", "head", "grep", "wc", "cat"];

/// The parts of a pipeline, or `None` for a command that chains, substitutes or redirects to a
/// file, which Claude Code asks about whatever its rules say.
fn pipeline(command: &str) -> Option<Vec<String>> {
    let plain = command.replace("2>&1", "");
    if [";", "&&", "||", ">", "<", "`", "$("].iter().any(|operator| plain.contains(operator)) {
        return None;
    }
    Some(plain.split('|').map(|part| part.split_whitespace().collect::<Vec<_>>().join(" ")).collect())
}

/// Whether the project's local settings let `command` run without asking, as Claude Code reads
/// them: from the main checkout of the worktree's repository, each `Bash(<prefix> *)` rule allowing
/// the commands that start with its prefix.
fn allowed_by_rules(cwd: &Path, command: &str) -> bool {
    let Some(parts) = pipeline(command) else { return false };
    let Ok(common) = super::git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) else { return false };
    let Some(main) = Path::new(&common).parent() else { return false };
    let Ok(text) = fs::read_to_string(main.join(".claude/settings.local.json")) else { return false };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else { return false };
    let prefixes: Vec<String> = settings["permissions"]["allow"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rule| rule.as_str()?.strip_prefix("Bash(")?.strip_suffix(" *)").map(str::to_owned))
        .collect();
    parts.iter().all(|part| {
        READ_ONLY.contains(&part.split(' ').next().unwrap_or_default())
            || prefixes.iter().any(|prefix| part == prefix || part.starts_with(&format!("{prefix} ")))
    })
}

/// The rules Claude Code suggests with its permission request: one per part of the command that
/// is not read-only, by its first two words.
fn suggestions(command: &str) -> Value {
    let rules: Vec<Value> = command
        .split(['|', ';', '&'])
        .map(|part| part.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .filter(|head| !head.is_empty() && !READ_ONLY.contains(&head.split(' ').next().unwrap_or_default()))
        .map(|head| json!({"toolName": "Bash", "ruleContent": format!("{head} *")}))
        .collect();
    json!([{"type": "addRules", "rules": rules, "behavior": "allow", "destination": "localSettings"}])
}

/// What `alondra-sandbox fake node` asks of a native session.
pub fn command(fakes: &Fakes, session: &Session, verb: &str, step: Option<String>) -> Result<()> {
    let steps = match step {
        Some(json) => Step::parse_all(&json).context("--step is one step, or a list of steps, as JSON")?,
        None => match verb {
            "work" => Step::work(fakes.prs()?.keys().max().copied().unwrap_or(0) + 1, &format!("{}.txt", session.short)),
            "deliver" => vec![Step::Deliver { need: "Review the change".into(), pr: Some(current_pr(fakes, &session.cwd, &session.short)?), is_final: true }],
            "ask" => vec![Step::Ask {
                id: format!("ask-{}-{}", session.short, hex(&random::<3>()?)),
                need: "Pick how to name the file".into(),
                options: vec!["Short name".into(), "Long name".into()],
            }],
            "cleanup" => vec![Step::Cleanup],
            "end" => vec![Step::End],
            "die" => vec![Step::Die],
            other => bail!("unknown verb {other}: work, deliver, ask, cleanup, end or die"),
        },
    };
    for outcome in order(fakes, session, &Order { steps, end_turn: false })? {
        println!("{} {}: {}", if outcome.ok { "ok" } else { "refused" }, serde_json::to_string(&outcome.step)?, outcome.text);
    }
    Ok(())
}

/// The PR of the branch checked out in `dir`.
pub fn current_pr(fakes: &Fakes, dir: &Path, short: &str) -> Result<u64> {
    let branch = super::git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    fakes.prs()?.values().find(|pr| pr.branch == branch).map(|pr| pr.number).ok_or_else(|| anyhow!("{short} has no PR yet; use work"))
}
