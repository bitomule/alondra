//! The `claude --bg` sessions an earlier release planned to move to the Agent SDK, as the rest of
//! the engine meets them: what is said to such a node follows it to its SDK node once its move is
//! done, and how Alondra asks Claude Code about a session and stops it. The moves themselves are
//! carried out above the engine (`bg_handover`).
use crate::live::LiveOf as _;
use alondra_core::clock;
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{Handover, HandoverProcess, HandoverStep};
use crate::node_close::Answer;
use crate::node_push::Unreachable;
use alondra_domain_rules::*;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// How often the worker looks for inputs waiting for a node at rest when nothing in the domain
/// changes, and goes on with a move that waited.
pub const EVERY: Duration = Duration::from_secs(10);

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
            use crate::node_close::Sessions;
            let listing = crate::node_close::Machine.agents().await?;
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
pub(crate) fn still_running(agents_json: &str, short: &str) -> bool {
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
    #[cfg(any(test, feature = "test-hooks"))]
    pub crash_after: std::sync::Mutex<Option<HandoverStep>>,
    #[cfg(any(test, feature = "test-hooks"))]
    pub turn_deadline: std::sync::Mutex<Option<std::time::Duration>>,
}

/// How long a `claude --bg` node in a turn gets to end it before Alondra interrupts it to stop it.
const TURN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

impl Handovers {
    pub(crate) fn turn_deadline(&self) -> std::time::Duration {
        #[cfg(any(test, feature = "test-hooks"))]
        if let Some(deadline) = *self.turn_deadline.lock().unwrap() {
            return deadline;
        }
        TURN_DEADLINE
    }

    pub fn sessions(&self) -> Arc<dyn BgSessions> {
        Arc::clone(self.sessions.get_or_init(|| Arc::new(ClaudeSessions)))
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn use_sessions(&self, sessions: Arc<dyn BgSessions>) {
        assert!(self.sessions.set(sessions).is_ok(), "the sessions are set once");
    }
}

pub fn handover_of(domain: &DomainRuntime, agent: &AgentId) -> Option<Handover> {
    match domain.store.lock_or_exit("the domain store").handovers() {
        Ok(handovers) => handovers.into_iter().rev().find(|handover| handover.node == *agent),
        Err(error) => {
            log!("could not read the SDK handover journal: {error:#}");
            None
        }
    }
}

/// The task and SDK node that took over `agent`'s session, once its move is done.
pub fn successor(domain: &DomainRuntime, agent: &AgentId) -> Option<(TaskId, AgentId)> {
    handover_of(domain, agent).filter(|handover| handover.step == HandoverStep::Done).and_then(|handover| Some((handover.task, handover.agent?)))
}

/// Whether `agent`'s session moved, or is moving, to the SDK: nothing restarts or removes it.
pub(crate) fn moved(domain: &DomainRuntime, agent: &AgentId) -> bool {
    handover_of(domain, agent).is_some()
}

/// Whether the outbox messages for `agent` wait for its move instead of reaching it through its
/// node mod.
pub(crate) fn holds_messages(domain: &DomainRuntime, agent: &AgentId) -> bool {
    domain.sdk().is_some() && moved(domain, agent)
}

/// How `tell` reached the node.
pub enum Told {
    /// Down its node mod's connection: resolved once the session takes it.
    Pushed(tokio::sync::oneshot::Receiver<()>),
    /// Through the SDK: it opens the node's session there, or follows it to the SDK node.
    Moving,
}

/// Tells the `claude --bg` node `session` `text` from `from`: down its connection, or to the SDK
/// node its move leads to.
pub fn tell(domain: &DomainRuntime, session: &str, from: &str, text: &str, as_user: bool) -> Result<Told, Unreachable> {
    let agent = AgentId(session.to_owned());
    if domain.sdk().is_some() {
        let stamped = crate::node_push::stamped(from, alondra_core::now_millis(), text);
        if let Some(handover) = handover_of(domain, &agent) {
            follow(domain, handover, stamped);
            return Ok(Told::Moving);
        }
    }
    domain.live().pushes.message(session, from, text, as_user).map(Told::Pushed)
}

/// What reaches a node that already moves goes to the SDK node once the move is done.
fn follow(domain: &DomainRuntime, handover: Handover, text: String) {
    let Some((_, runtime)) = domain.sdk() else { return };
    tokio::spawn(async move {
        let node = handover.node.clone();
        let mut changed = runtime.subscribe();
        loop {
            match handover_of(&runtime, &node) {
                Some(Handover { step: HandoverStep::Done, task, .. }) => {
                    let command = format!("tell-moved-{}", crate::auth::random_hex(16));
                    if let Err(error) = crate::domain_messages::tell_sdk_node(&runtime, &task, crate::node_push::ALONDRA, &text, &command, false) {
                        log!("what was said to {node} while it moved did not reach its SDK node: {}", error.message);
                    }
                    return;
                }
                Some(_) => {}
                None => return,
            }
            tokio::select! {
                _ = changed.changed() => {}
                () = clock::sleep(EVERY) => {}
            }
        }
    });
}


crate::extension!(Handovers);

pub trait HandoversOf {
    fn handovers(&self) -> &Handovers;
}

impl HandoversOf for crate::domain_runtime::DomainRuntime {
    fn handovers(&self) -> &Handovers {
        self.extension()
    }
}
