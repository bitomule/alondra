//! Claude Code's `claude --bg` sessions on this Mac as the engine meets them: running `claude`,
//! listing and stopping a session, and the moves of such sessions to the Agent SDK that an earlier
//! release planned, one at a time.
use crate::Runtime;
use crate::domain_store::{Handover, HandoverProcess, HandoverStep};
use crate::locks::Poisonable as _;
use alondra_core::clock;
use alondra_domain_rules::{AgentId, TaskId};
use alondra_engine_ports::node_close::{Answer, Ran, agents_args, listing};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// How often the worker looks for inputs waiting for a node at rest when nothing in the domain
/// changes, and goes on with a move that waited.
pub const EVERY: Duration = Duration::from_secs(10);

/// The JSON of `claude agents --json --all` on this Mac.
pub async fn agents() -> Result<String, String> {
    listing(claude(agents_args()).await?)
}

/// Runs `claude` on this Mac; Err when it could not run or did not answer in 60 s.
pub async fn claude(args: Vec<String>) -> Result<Ran, String> {
    let run = tokio::process::Command::new("claude").args(&args).kill_on_drop(true).output();
    match clock::timeout(Duration::from_secs(60), run).await {
        Ok(Ok(out)) => Ok(Ran {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("claude {} did not answer in 60 s", args.join(" "))),
    }
}

/// What a move asks of Claude Code about a `claude --bg` session, so a test can stand in for it.
pub trait BgSessions: Send + Sync + 'static {
    /// The process Claude Code lists for `session` now, if one runs.
    fn listed<'a>(&'a self, session: &'a str) -> Answer<'a, Result<Option<HandoverProcess>, String>>;
    /// Asks Claude Code to stop `session` and waits until it no longer lists it running.
    fn stop<'a>(&'a self, session: &'a str) -> Answer<'a, Result<(), String>>;
    /// Whether `process` is still that same process and has not exited.
    fn alive(&self, process: &HandoverProcess) -> bool;
}

/// Claude Code on this Mac.
pub struct ClaudeSessions;

pub fn member(process: &HandoverProcess) -> alondra_process_group::Member {
    alondra_process_group::Member { pid: process.pid, started: process.started, zombie: false }
}

pub fn identity(member: alondra_process_group::Member) -> HandoverProcess {
    HandoverProcess { pid: member.pid, started: member.started }
}

impl BgSessions for ClaudeSessions {
    fn listed<'a>(&'a self, session: &'a str) -> Answer<'a, Result<Option<HandoverProcess>, String>> {
        Box::pin(async move {
            let listing = agents().await?;
            let list: Vec<serde_json::Value> = listing.find('[').and_then(|at| serde_json::from_str(&listing[at..]).ok())
                .ok_or_else(|| "claude agents did not answer with its list of sessions".to_owned())?;
            let pid = list.iter().find(|row| row["sessionId"].as_str() == Some(session)).and_then(|row| row["pid"].as_i64());
            match pid.and_then(|pid| i32::try_from(pid).ok()) {
                Some(pid) => alondra_process_group::process(pid).map(|found| found.map(identity)).map_err(|error| error.to_string()),
                None => Ok(None),
            }
        })
    }

    fn stop<'a>(&'a self, session: &'a str) -> Answer<'a, Result<(), String>> {
        Box::pin(async move {
            let short: String = session.chars().take(8).collect();
            stop_checked(&short).await
        })
    }

    fn alive(&self, process: &HandoverProcess) -> bool {
        alondra_process_group::alive(&member(process)).unwrap_or(true)
    }
}

/// Stops a `claude --bg` session for good and says whether it did: `claude stop` answered and
/// `claude agents` no longer shows it running.
async fn stop_checked(short: &str) -> Result<(), String> {
    let stop = tokio::process::Command::new("claude").args(["stop", short]).kill_on_drop(true)
        .stdin(std::process::Stdio::null()).output();
    match clock::timeout(std::time::Duration::from_secs(20), stop).await {
        Ok(Ok(out)) if out.status.success() => {}
        Ok(Ok(out)) => return Err(format!("claude stop {short} failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
        Ok(Err(error)) => return Err(format!("claude stop {short} failed: {error}")),
        Err(_) => return Err(format!("claude stop {short} did not answer in 20 s")),
    }
    for _ in 0..5 {
        let list = tokio::process::Command::new("claude").args(["agents", "--json", "--all"]).kill_on_drop(true)
            .stdin(std::process::Stdio::null()).output();
        let listing = match clock::timeout(std::time::Duration::from_secs(20), list).await {
            Ok(Ok(out)) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            Ok(Ok(out)) => return Err(format!("claude agents failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
            Ok(Err(error)) => return Err(format!("claude agents failed: {error}")),
            Err(_) => return Err("claude agents did not answer in 20 s".into()),
        };
        if !still_running(&listing, short) {
            return Ok(());
        }
        clock::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(format!("claude agents still shows {short} running after claude stop"))
}

/// Whether `claude agents --json` lists `short` in a state that still runs a turn or waits for one.
pub fn still_running(agents_json: &str, short: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(agents_json).ok().and_then(|list| list.as_array().cloned()).is_some_and(|list| list.iter().any(|agent| {
        agent.get("id").and_then(serde_json::Value::as_str) == Some(short)
            && matches!(agent.get("state").and_then(serde_json::Value::as_str), Some("working" | "idle" | "blocked"))
    }))
}

/// The daemon's moves: one advances at a time.
#[derive(Default)]
pub struct Handovers {
    sessions: OnceLock<Arc<dyn BgSessions>>,
    pub moving: tokio::sync::Mutex<()>,
    /// A move stops right after recording this step, as a daemon that dies then does: the
    /// admission dies after its commit, before its host starts.
    pub crash_after: std::sync::Mutex<Option<HandoverStep>>,
    pub turn_deadline: std::sync::Mutex<Option<std::time::Duration>>,
}

/// How long a `claude --bg` node in a turn gets to end it before Alondra interrupts it to stop it.
const TURN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

impl Handovers {
    pub fn turn_deadline(&self) -> std::time::Duration {
        if let Some(deadline) = *self.turn_deadline.lock_or_recover("the turn deadline a test set") {
            return deadline;
        }
        TURN_DEADLINE
    }

    pub fn sessions(&self) -> Arc<dyn BgSessions> {
        Arc::clone(self.sessions.get_or_init(|| Arc::new(ClaudeSessions)))
    }

    pub fn use_sessions(&self, sessions: Arc<dyn BgSessions>) {
        assert!(self.sessions.set(sessions).is_ok(), "the sessions are set once");
    }
}

pub fn handover_of(domain: &dyn Runtime, agent: &AgentId) -> Option<Handover> {
    match domain.store().lock_or_exit("the domain store").handovers() {
        Ok(handovers) => handovers.into_iter().rev().find(|handover| handover.node == *agent),
        Err(error) => {
            log!("could not read the SDK handover journal: {error:#}");
            None
        }
    }
}

/// The task and SDK node that took over `agent`'s session, once its move is done.
pub fn successor(domain: &dyn Runtime, agent: &AgentId) -> Option<(TaskId, AgentId)> {
    handover_of(domain, agent).filter(|handover| handover.step == HandoverStep::Done).and_then(|handover| Some((handover.task, handover.agent?)))
}

crate::extension!(Handovers);

pub trait HandoversOf {
    fn handovers(&self) -> &Handovers;
}

impl<R: alondra_engine_ports::Runtime + ?Sized> HandoversOf for R {
    fn handovers(&self) -> &Handovers {
        alondra_engine_ports::extension(self.as_runtime())
    }
}
