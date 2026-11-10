//! A fake background session: what `claude --bg` starts. It reports and holds its connection
//! through the real `alondra node event|connect|ack`, as the node plugin does, acts through the
//! real `alondra mcp-node` with the settings file Alondra wrote, and follows its task's script.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Fakes, append, flag_value, git, mcp, read_lines,
    script::{Step, TaskScript},
    work, write_json,
};
use crate::sandbox::{hex, now_ms, random};

/// What `claude --version` answers; Alondra needs mods (2.1.321+).
pub const VERSION: &str = "2.1.321";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    /// `claude stop`, or the session ended: `claude respawn` brings it back.
    Stopped,
    /// Killed without a word: `claude respawn` brings it back.
    Dead,
    Removed,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Node {
    pub short: String,
    pub session_id: String,
    pub name: String,
    pub alondra_home: PathBuf,
    pub claude_home: PathBuf,
    pub cwd: PathBuf,
    pub settings: PathBuf,
    pub prompt: String,
    pub task_id: Option<String>,
    pub worktree: Option<PathBuf>,
    pub pid: Option<u32>,
    pub state: State,
    /// How many times it was started; more than one means it was respawned.
    pub starts: u32,
}

/// One line its connection carried.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Pushed {
    pub at: u64,
    pub id: Option<String>,
    pub text: Option<String>,
    pub interrupt: bool,
}

/// What a step did.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Outcome {
    pub at: u64,
    pub step: Step,
    pub ok: bool,
    pub text: String,
}

impl Fakes {
    fn nodes(&self) -> PathBuf {
        self.dir.join("nodes")
    }

    fn node_dir(&self, short: &str) -> PathBuf {
        self.nodes().join(short)
    }

    pub fn node(&self, short: &str) -> Result<Node> {
        let path = self.node_dir(short).join("node.json");
        Ok(serde_json::from_slice(&fs::read(&path).with_context(|| format!("no fake node {short}"))?)?)
    }

    fn save(&self, node: &Node) -> Result<()> {
        write_json(&self.node_dir(&node.short).join("node.json"), node)
    }

    pub fn all_nodes(&self) -> Result<Vec<Node>> {
        let mut nodes = vec![];
        let Ok(entries) = fs::read_dir(self.nodes()) else { return Ok(nodes) };
        for entry in entries {
            let short = entry?.file_name().to_string_lossy().into_owned();
            if let Ok(node) = self.node(&short) {
                nodes.push(node);
            }
        }
        nodes.sort_by(|a, b| a.short.cmp(&b.short));
        Ok(nodes)
    }

    /// The latest node of task `task_id`.
    pub fn node_of(&self, task_id: &str) -> Result<Option<Node>> {
        Ok(self.all_nodes()?.into_iter().filter(|node| node.task_id.as_deref() == Some(task_id)).max_by_key(|node| node.starts_key()))
    }
}

impl Node {
    fn starts_key(&self) -> (bool, String) {
        (self.state != State::Removed, self.short.clone())
    }

    fn launch_key(&self) -> String {
        self.settings.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn workdir(&self) -> &Path {
        self.worktree.as_deref().unwrap_or(&self.cwd)
    }

    /// The `env` block of the settings file Alondra wrote, as Claude Code hands it to the session.
    fn env(&self) -> Result<Vec<(String, String)>> {
        let settings: Value = serde_json::from_slice(&fs::read(&self.settings).with_context(|| format!("read {}", self.settings.display()))?)?;
        let mut env: Vec<(String, String)> = settings["env"]
            .as_object()
            .map(|env| env.iter().filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned()))).collect())
            .unwrap_or_default();
        env.push(("ALONDRA_HOME".into(), self.alondra_home.display().to_string()));
        Ok(env)
    }

    fn has_capability(&self) -> bool {
        self.env().is_ok_and(|env| env.iter().any(|(key, _)| key == "ALONDRA_DOMAIN_CAPABILITY"))
    }
}

/// `claude --bg … -- <prompt>`: records the session and starts its process in the background.
pub fn launch(fakes: &Fakes, args: &[String]) -> Result<i32> {
    let name = flag_value(args, "--name").unwrap_or("node").to_owned();
    let settings = PathBuf::from(flag_value(args, "--settings").context("claude --bg without --settings")?);
    let prompt = args.iter().position(|arg| arg == "--").and_then(|at| args.get(at + 1)).cloned().unwrap_or_default();
    let alondra_home = PathBuf::from(std::env::var("ALONDRA_HOME").context("claude --bg without ALONDRA_HOME")?);
    let claude_home = PathBuf::from(std::env::var("CLAUDE_HOME").context("the sandbox's claude needs CLAUDE_HOME, so it never touches ~/.claude")?);
    let short = hex(&random::<4>()?);
    let tail = hex(&random::<12>()?);
    let session_id = format!("{short}-{}-4{}-8{}-{}", &tail[0..4], &tail[4..7], &tail[7..10], &tail[10..22]);
    let task_id = fakes.task_script_in(&prompt)?.map(|task| task.task_id);
    let node = Node {
        short: short.clone(),
        session_id,
        name: name.clone(),
        alondra_home,
        claude_home,
        cwd: std::env::current_dir()?,
        settings,
        prompt,
        task_id,
        worktree: None,
        pid: None,
        state: State::Running,
        starts: 0,
    };
    fakes.save(&node)?;
    start(fakes, &short)?;
    println!("backgrounded · {short} · {name}");
    Ok(0)
}

/// Starts the session's process in its own process group, so stopping it takes its helpers too.
fn start(fakes: &Fakes, short: &str) -> Result<()> {
    let log = fs::OpenOptions::new().create(true).append(true).open(fakes.node_dir(short).join("node.log"))?;
    Command::new(fakes.tool())
        .args(["--sandbox"])
        .arg(&fakes.sandbox)
        .args(["fake-node", short])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .context("start the fake node")?;
    Ok(())
}

/// The session's process: worktree, `session.start`, its connection, its script.
pub fn run(fakes: &Fakes, short: &str) -> Result<()> {
    let mut node = fakes.node(short)?;
    if node.state == State::Removed {
        bail!("{short} was removed");
    }
    let fresh = node.starts == 0;
    if node.worktree.is_none() && git(&node.cwd, &["rev-parse", "--show-toplevel"]).is_ok() {
        let worktree = fakes.dir.join("worktrees").join(short);
        git(&node.cwd, &["worktree", "add", "-b", &format!("node/{short}"), worktree.to_str().context("worktree path")?])?;
        node.worktree = Some(worktree);
    }
    write_job(&node, "working")?;
    node.pid = Some(std::process::id());
    node.state = State::Running;
    node.starts += 1;
    fakes.save(&node)?;
    event(fakes, &node, json!({"event": "session.start", "version": VERSION, "cwd": node.workdir(), "launch_key": node.launch_key()}))?;
    eprintln!("{} session.start reported", now_ms());
    let script = match &node.task_id {
        Some(task) => fakes.task_script(task)?,
        None => None,
    };
    let listener = {
        let (fakes, node, script) = (fakes.clone(), node.clone(), script.clone());
        std::thread::spawn(move || listen(&fakes, &node, script.as_ref()))
    };
    hook(fakes, &node, "SessionStart", json!({"source": if fresh { "startup" } else { "resume" }}))?;
    eprintln!("{} SessionStart hooked", now_ms());
    if fresh {
        wait_until_bound(&node)?;
        eprintln!("{} bound", now_ms());
        hook(fakes, &node, "UserPromptSubmit", json!({"prompt": node.prompt}))?;
        eprintln!("{} prompt submitted", now_ms());
        event(fakes, &node, json!({"event": "turn.start", "turn_id": "launch"}))?;
        // A session starts by reading around. Alondra assigns the task just after it writes the
        // bearer, and starts it on the session's next hook, so these reads also start it.
        let began = now_ms();
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(500));
            if stopped_since(fakes, &node, began)? {
                break;
            }
            let read = json!({"tool_name": "Read", "tool_input": {"file_path": "README.md"}});
            hook(fakes, &node, "PreToolUse", read.clone())?;
            hook(fakes, &node, "PostToolUse", read)?;
        }
        if let Some(script) = script.as_ref().filter(|script| !script.on_start.is_empty()) {
            for step in &script.on_start {
                act(fakes, &node, step)?;
            }
            end_turn(fakes, &node, "launch")?;
        }
    } else {
        write_job(&node, "idle")?;
    }
    listener.join().map_err(|_| anyhow!("the connection thread panicked"))?
}

/// The job file Claude Code keeps for a background session, which Alondra reads.
fn write_job(node: &Node, state: &str) -> Result<()> {
    let job = json!({
        "state": state, "detail": format!("sandbox node {state}"), "sessionId": node.session_id, "resumeSessionId": node.session_id,
        "updatedAt": rfc3339(now_ms()), "worktreePath": node.worktree, "worktreeBranch": format!("node/{}", node.short),
        "cwd": node.cwd, "name": node.name,
    });
    write_json(&node.claude_home.join("jobs").join(&node.short).join("state.json"), &job)
}

pub(crate) fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rest / 3600, rest % 3600 / 60, rest % 60, ms % 1000)
}

/// The milliseconds of a UTC time as `rfc3339` writes it.
pub(super) fn parse_rfc3339(text: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (y, m, d) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (h, min, s) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let ms = if text.as_bytes().get(19) == Some(&b'.') { number(20..23)? } else { 0 };
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    u64::try_from(((days * 86_400 + h * 3600 + min * 60 + s) * 1000) + ms).ok()
}

/// Days since 1970-01-01 as a calendar date (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Alondra writes the bearer into the settings file once it bound the session.
fn wait_until_bound(node: &Node) -> Result<()> {
    crate::sandbox::wait_for("Alondra to bind the session", Duration::from_secs(40), || Ok(node.has_capability().then_some(())))
}

/// Whether the session ended a turn since `at`: it reads nothing more after that.
fn stopped_since(fakes: &Fakes, node: &Node, at: u64) -> Result<bool> {
    let hooks: Vec<Value> = read_lines(&fakes.node_dir(&node.short).join("hooks.jsonl"))?;
    Ok(hooks.iter().any(|hook| hook["hook"] == "Stop" && hook["at"].as_u64().is_some_and(|when| when >= at)))
}

/// The session ends the turn it is on, as Claude does once it has acted.
pub fn finish_turn(fakes: &Fakes, node: &Node) -> Result<()> {
    end_turn(fakes, node, "journey")
}

/// The turn ends: the Stop hook, then the mod's report.
fn end_turn(fakes: &Fakes, node: &Node, turn: &str) -> Result<()> {
    // A blocked stop makes Claude Code go on and stop again, with stop_hook_active set.
    if hook(fakes, node, "Stop", json!({"stop_hook_active": false}))?["decision"] == "block" {
        hook(fakes, node, "Stop", json!({"stop_hook_active": true}))?;
    }
    write_job(node, "idle")?;
    event(fakes, node, json!({"event": "turn.complete", "turn_id": turn, "answer": "Done."}))
}

/// How long the plugin's hooks.json lets each of its command hooks run.
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// Waits up to `limit` for a helper: whether it succeeded, and what it printed. A helper that
/// hangs is killed and the hang is the error.
fn finish(mut child: std::process::Child, what: &str, limit: Duration) -> Result<(bool, String, String)> {
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            bail!("{what} did not answer within {} s", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        std::io::Read::read_to_string(&mut out, &mut stdout)?;
    }
    if let Some(mut err) = child.stderr.take() {
        std::io::Read::read_to_string(&mut err, &mut stderr)?;
    }
    Ok((status.success(), stdout, stderr))
}

/// What Claude Code runs for a classic hook: `alondra hook` with the event on stdin. As in Claude
/// Code, a hook that fails or outlives its timeout is a non-blocking error: the session goes on.
fn hook(fakes: &Fakes, node: &Node, name: &str, mut payload: Value) -> Result<Value> {
    payload["session_id"] = json!(node.session_id);
    payload["hook_event_name"] = json!(name);
    payload["cwd"] = json!(node.workdir());
    let mut child = Command::new(fakes.alondra())
        .arg("hook")
        .envs(node.env().unwrap_or_default())
        .env("ALONDRA_HOME", &node.alondra_home)
        .env("ALONDRA_LAUNCHED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().context("hook stdin")?.write_all(payload.to_string().as_bytes())?;
    let reply = match finish(child, &format!("alondra hook {name}"), HOOK_TIMEOUT) {
        Ok((true, stdout, _)) => serde_json::from_str(stdout.trim()).unwrap_or(Value::Null),
        Ok((false, _, stderr)) => {
            eprintln!("{} alondra hook {name} failed: {}", now_ms(), stderr.trim());
            json!({"error": stderr.trim()})
        }
        Err(error) => {
            eprintln!("{} {error:#}", now_ms());
            json!({"error": format!("{error:#}")})
        }
    };
    append(&fakes.node_dir(&node.short).join("hooks.jsonl"), &json!({"at": now_ms(), "hook": name, "reply": reply}))?;
    Ok(reply)
}

/// Holds `alondra node connect`, redialling like the plugin while the session lives.
fn listen(fakes: &Fakes, node: &Node, script: Option<&TaskScript>) -> Result<()> {
    loop {
        match connect_once(fakes, node, script) {
            Ok(()) => {}
            Err(error) => eprintln!("connection: {error:#}"),
        }
        if fakes.node(&node.short)?.state != State::Running {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn connect_once(fakes: &Fakes, node: &Node, script: Option<&TaskScript>) -> Result<()> {
    let hello = json!({"session_id": node.session_id, "version": VERSION, "cwd": node.workdir(), "launch_key": node.launch_key(), "working": false});
    let mut helper = Command::new(fakes.alondra())
        .args(["node", "connect"])
        .env("ALONDRA_HOME", &node.alondra_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    helper.stdin.take().context("connect stdin")?.write_all(hello.to_string().as_bytes())?;
    let reader = BufReader::new(helper.stdout.take().context("connect stdout")?);
    for line in reader.lines() {
        let line = line?;
        if line == "connected" || line.trim().is_empty() {
            continue;
        }
        let push: Value = serde_json::from_str(&line).with_context(|| format!("not JSON: {line}"))?;
        let pushed = Pushed {
            at: now_ms(),
            id: push["id"].as_str().map(str::to_owned),
            text: push["text"].as_str().map(str::to_owned),
            interrupt: push["interrupt"] == true,
        };
        append(&fakes.node_dir(&node.short).join("pushed.jsonl"), &serde_json::to_value(&pushed)?)?;
        if let (Some(id), Some(text)) = (&pushed.id, &pushed.text) {
            write_job(node, "working")?;
            hook(fakes, node, "UserPromptSubmit", json!({"prompt": text}))?;
            ack(fakes, node, id)?;
            event(fakes, node, json!({"event": "turn.start", "turn_id": id}))?;
            if respond(fakes, node, script, text)? {
                end_turn(fakes, node, id)?;
            }
        }
    }
    let _ = helper.wait();
    Ok(())
}

/// What the scripted session does with a message: clean up when Alondra asks, rework on feedback.
/// Whether its turn ends now: a restarted session told to continue, and feedback with no scripted
/// rework, stay working until the journey drives the work and ends the turn.
fn respond(fakes: &Fakes, node: &Node, script: Option<&TaskScript>, text: &str) -> Result<bool> {
    let Some(script) = script else { return Ok(true) };
    if text.contains("continue your task") {
        return Ok(false);
    }
    if text.contains("cleanup_done") {
        if script.cleans_up {
            act(fakes, node, &Step::Cleanup)?;
        }
    } else if is_feedback(text) {
        if script.on_feedback.is_empty() {
            return Ok(false);
        }
        for step in &script.on_feedback {
            act(fakes, node, step)?;
        }
    }
    Ok(true)
}

/// Alondra hands a result back (changes, a question or reply about it, a failed merge) with the
/// instruction to present it again.
fn is_feedback(text: &str) -> bool {
    text.contains("present the result again") || text.contains("present_result again")
}

fn event(fakes: &Fakes, node: &Node, mut report: Value) -> Result<()> {
    report["session_id"] = json!(node.session_id);
    cli(fakes, node, &["node", "event"], &report)
}

fn ack(fakes: &Fakes, node: &Node, id: &str) -> Result<()> {
    cli(fakes, node, &["node", "ack", id], &json!({"session_id": node.session_id}))
}

fn cli(fakes: &Fakes, node: &Node, args: &[&str], stdin: &Value) -> Result<()> {
    let mut child = Command::new(fakes.alondra())
        .args(args)
        .env("ALONDRA_HOME", &node.alondra_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().context("stdin")?.write_all(stdin.to_string().as_bytes())?;
    let (ok, _, stderr) = finish(child, &format!("alondra {}", args.join(" ")), Duration::from_secs(30))?;
    if !ok {
        bail!("alondra {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

/// Does one step as the session would and writes down what happened.
pub fn act(fakes: &Fakes, node: &Node, step: &Step) -> Result<Outcome> {
    let tool = work::tool_of(step);
    let use_id = format!("toolu_{}", hex(&random::<6>()?));
    if tool.is_some() {
        write_job(node, "working")?;
    }
    if let Some((name, input)) = &tool {
        hook(fakes, node, "PreToolUse", json!({"tool_name": name, "tool_input": input, "tool_use_id": use_id}))?;
        event(fakes, node, json!({"event": "tool.start", "tool": name, "tool_use_id": use_id, "input": input}))?;
    }
    let result = perform(fakes, node, step);
    if let Some((name, input)) = &tool {
        hook(fakes, node, "PostToolUse", json!({"tool_name": name, "tool_input": input, "tool_use_id": use_id}))?;
        event(fakes, node, json!({"event": "tool.end", "tool_use_id": use_id}))?;
    }
    let (ok, text) = match result {
        Ok(reply) => (!reply.refused, reply.text),
        Err(error) => (false, format!("{error:#}")),
    };
    let outcome = Outcome { at: now_ms(), step: step.clone(), ok, text };
    append(&fakes.node_dir(&node.short).join("steps.jsonl"), &serde_json::to_value(&outcome)?)?;
    Ok(outcome)
}

fn perform(fakes: &Fakes, node: &Node, step: &Step) -> Result<mcp::Reply> {
    let env = || node.env();
    let worker = work::Worker { task: node.task_id.as_deref(), dir: node.workdir(), short: &node.short, env: &env };
    if let Some(result) = work::perform(fakes, &worker, step) {
        return result;
    }
    match step {
        Step::Speak { said, behind, listed } => {
            let turn = format!("speak-{}", hex(&random::<3>()?));
            write_job(node, "working")?;
            hook(fakes, node, "UserPromptSubmit", json!({"prompt": "go on"}))?;
            event(fakes, node, json!({"event": "turn.start", "turn_id": turn}))?;
            let mut stop = json!({"stop_hook_active": false, "last_assistant_message": said});
            if let Some(command) = behind {
                stop["background_tasks"] = json!([{"id": format!("bg-{turn}"), "type": "shell", "command": command, "status": "running"}]);
            }
            if hook(fakes, node, "Stop", stop.clone())?["decision"] == "block" {
                stop["stop_hook_active"] = json!(true);
                hook(fakes, node, "Stop", stop)?;
            }
            write_job(node, listed.as_deref().unwrap_or("blocked"))?;
            event(fakes, node, json!({"event": "turn.complete", "turn_id": turn, "answer": said}))?;
            Ok(work::done("said"))
        }
        Step::End => {
            end(fakes, &node.short, State::Stopped)?;
            Ok(work::done("ended"))
        }
        Step::Die => {
            kill(fakes, &node.short, State::Dead)?;
            Ok(work::done("killed"))
        }
        Step::OwnTurn { .. } | Step::LeavePlanMode { .. } | Step::EndOwnTurn | Step::BackgroundEndsUnheard | Step::RateLimit { .. } | Step::Emit { .. } | Step::Elicit { .. } => Err(anyhow!("only a native Claude of the Agent SDK takes this step")),
        _ => unreachable!("work::perform takes every other step"),
    }
}

fn signal_group(pid: u32, signal: &str) {
    let _ = Command::new("kill").args([signal, &format!("-{pid}")]).stderr(Stdio::null()).status();
}

/// Kills the session's process group without telling Alondra, and marks it `state`.
pub fn kill(fakes: &Fakes, short: &str, state: State) -> Result<()> {
    let mut node = fakes.node(short)?;
    match state {
        State::Dead => write_job(&node, "crashed")?,
        State::Stopped => write_job(&node, "stopped")?,
        State::Running | State::Removed => {}
    }
    node.state = state;
    fakes.save(&node)?;
    if let Some(pid) = node.pid {
        signal_group(pid, "-KILL");
    }
    Ok(())
}

/// Ends the session as Claude Code does on exit, stop or rm: `session.end`, then its processes go.
pub fn end(fakes: &Fakes, short: &str, state: State) -> Result<()> {
    let node = fakes.node(short)?;
    if state == State::Stopped {
        write_job(&node, "stopped")?;
    }
    if node.state == State::Running {
        let _ = hook(fakes, &node, "SessionEnd", json!({"reason": "other"}));
        let _ = event(fakes, &node, json!({"event": "session.end", "reason": "other"}));
    }
    kill(fakes, short, state)
}

/// `claude rm`: ends the session and deletes its worktree and job.
pub fn remove(fakes: &Fakes, short: &str) -> Result<i32> {
    let Ok(node) = fakes.node(short) else {
        eprintln!("No background session {short}");
        return Ok(1);
    };
    end(fakes, short, State::Removed)?;
    if let Some(worktree) = &node.worktree {
        let _ = git(&node.cwd, &["worktree", "remove", "--force", worktree.to_str().unwrap_or_default()]);
    }
    let _ = fs::remove_dir_all(node.claude_home.join("jobs").join(short));
    println!("removed {short}");
    Ok(0)
}

/// `claude respawn`: starts a stopped or dead session again under the same id.
pub fn respawn(fakes: &Fakes, short: &str) -> Result<i32> {
    match fakes.node(short) {
        Ok(node) if node.state != State::Removed => {
            if let Some(pid) = node.pid {
                signal_group(pid, "-KILL");
            }
            start(fakes, short)?;
            Ok(0)
        }
        _ => {
            eprintln!("No background session {short}");
            Ok(1)
        }
    }
}

/// What the session's job file says it is doing: working mid-turn, idle between turns.
fn job_state(node: &Node) -> String {
    fs::read(node.claude_home.join("jobs").join(&node.short).join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|job| job["state"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "idle".into())
}

/// `claude agents --json --all`.
pub fn agents(fakes: &Fakes) -> Result<i32> {
    let list: Vec<Value> = fakes
        .all_nodes()?
        .into_iter()
        .filter(|node| node.state != State::Removed)
        .map(|node| {
            // Like Claude Code 2.1.300: a session that runs names its process.
            let (state, pid) = if node.state == State::Running { (job_state(&node), node.pid) } else { ("stopped".into(), None) };
            let mut row = json!({"id": node.short, "sessionId": node.session_id, "name": node.name, "state": state, "cwd": node.workdir()});
            if let Some(pid) = pid {
                row["pid"] = json!(pid);
            }
            row
        })
        .collect();
    println!("{}", serde_json::to_string(&list)?);
    Ok(0)
}

/// What `alondra-sandbox fake …` asks of the fake nodes and native sessions.
pub enum Order {
    List,
    Act { node: String, verb: String, step: Option<String> },
}

pub fn command(fakes: &Fakes, order: Order) -> Result<()> {
    match order {
        Order::List => {
            for node in fakes.all_nodes()? {
                let state = format!("{:?}", node.state).to_lowercase();
                println!("{}  {:<14} {:<8} {}", node.short, node.task_id.as_deref().unwrap_or("-"), state, node.name);
            }
            for session in fakes.all_sessions()? {
                let state = format!("{:?}", session.state).to_lowercase();
                println!("{}  {:<14} {:<8} sdk {}", session.short, session.task_id.as_deref().unwrap_or("-"), state, session.session_id);
            }
            Ok(())
        }
        Order::Act { node, verb, step } => {
            let session = match fakes.session(&node) {
                Ok(found) => Some(found),
                Err(_) => fakes.session_of(&node)?,
            };
            if let Some(session) = session.filter(|session| session.running()) {
                return super::native::command(fakes, &session, &verb, step);
            }
            let node = match fakes.node(&node) {
                Ok(found) => found,
                Err(_) => fakes.node_of(&node)?.ok_or_else(|| anyhow!("no fake node for {node}"))?,
            };
            let steps = match step {
                Some(json) => Step::parse_all(&json).context("--step is one step, or a list of steps, as JSON")?,
                None => steps_for(fakes, &node, &verb)?,
            };
            let ends = steps.iter().any(|step| matches!(step, Step::Speak { .. } | Step::End | Step::Die));
            for step in steps {
                let outcome = act(fakes, &node, &step)?;
                println!("{} {}: {}", if outcome.ok { "ok" } else { "refused" }, serde_json::to_string(&step)?, outcome.text);
            }
            if !ends {
                finish_turn(fakes, &node)?;
            }
            Ok(())
        }
    }
}

/// The PR of the branch checked out in the node's worktree.
pub fn current_pr(fakes: &Fakes, node: &Node) -> Result<u64> {
    let branch = git(node.workdir(), &["rev-parse", "--abbrev-ref", "HEAD"])?;
    fakes.prs()?.values().find(|pr| pr.branch == branch).map(|pr| pr.number).ok_or_else(|| anyhow!("{} has no PR yet; use work", node.short))
}

fn steps_for(fakes: &Fakes, node: &Node, verb: &str) -> Result<Vec<Step>> {
    Ok(match verb {
        "work" => Step::work(fakes.prs()?.keys().max().copied().unwrap_or(0) + 1, &format!("{}.txt", node.short)),
        "deliver" => vec![Step::Deliver { need: "Review the change".into(), pr: Some(current_pr(fakes, node)?), is_final: true }],
        "ask" => vec![Step::Ask {
            id: format!("ask-{}-{}", node.short, hex(&random::<3>()?)),
            need: "Pick how to name the file".into(),
            options: vec!["Short name".into(), "Long name".into()],
        }],
        "cleanup" => vec![Step::Cleanup],
        "end" => vec![Step::End],
        "die" => vec![Step::Die],
        other => bail!("unknown verb {other}: work, deliver, ask, cleanup, end or die"),
    })
}
