//! Once a daemon has an SDK backend, each `claude --bg` node moves to the Agent SDK: the same Claude
//! session, in the same folder, resumed through the SDK. A node at rest (its turn over, no work of
//! its own, nothing handed to it it has not taken) moves when the next input for it arrives, and
//! that input opens its session. A node Alondra brings back (its session lost or stalled, or out of
//! a pause) moves instead of being restarted, with the words that bring it back. A node in a turn
//! stays on `claude --bg` until it rests: facts decide, never the time.
//!
//! Each move is journaled in domain.db: planned with its input, the `claude --bg` process stopped
//! and its exit seen by the process's identity, the SDK execution admitted in the transaction
//! that records it, then the old node closed. A daemon that comes back goes on from the last step
//! recorded, so no step's effect happens twice, and the task never has two live owners.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{Handover, HandoverPlan, HandoverProcess, HandoverStep};
use crate::node_close::Answer;
use crate::node_push::Unreachable;
use alondra_core::domain::*;
use serde::Serialize;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// How often the worker looks for inputs waiting for a node at rest when nothing in the domain
/// changes, and goes on with a move that waited.
const EVERY: Duration = Duration::from_secs(10);

/// What a move asks of Claude Code about a `claude --bg` session, so a test can stand in for it.
pub(crate) trait BgSessions: Send + Sync + 'static {
    /// The process that runs `session` as Alondra sees it now, through its node mod's connection.
    fn seen(&self, domain: &DomainRuntime, session: &str) -> Option<HandoverProcess>;
    /// The process Claude Code lists for `session` now, if one runs.
    fn listed<'a>(&'a self, session: &'a str) -> Answer<'a, Result<Option<HandoverProcess>, String>>;
    /// Asks Claude Code to stop `session` and waits until it no longer lists it running.
    fn stop<'a>(&'a self, session: &'a str) -> Answer<'a, Result<(), String>>;
    /// Whether `process` is still that same process and has not exited.
    fn alive(&self, process: &HandoverProcess) -> bool;
}

/// Claude Code on this Mac.
pub(crate) struct ClaudeSessions;

fn member(process: &HandoverProcess) -> alondra_process_group::Member {
    alondra_process_group::Member { pid: process.pid, started: process.started, zombie: false }
}

fn identity(member: alondra_process_group::Member) -> HandoverProcess {
    HandoverProcess { pid: member.pid, started: member.started }
}

impl BgSessions for ClaudeSessions {
    fn seen(&self, domain: &DomainRuntime, session: &str) -> Option<HandoverProcess> {
        let connection = domain.live.mods.session(session)?.peer_pid?;
        alondra_process_group::parent(i32::try_from(connection).ok()?).ok().flatten().map(identity)
    }

    fn listed<'a>(&'a self, session: &'a str) -> Answer<'a, Result<Option<HandoverProcess>, String>> {
        Box::pin(async move {
            use crate::node_close::Sessions;
            let listing = crate::node_close::Machine.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await?;
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
    match tokio::time::timeout(std::time::Duration::from_secs(20), stop).await {
        Ok(Ok(out)) if out.status.success() => {}
        Ok(Ok(out)) => return Err(format!("claude stop {short} failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
        Ok(Err(error)) => return Err(format!("claude stop {short} failed: {error}")),
        Err(_) => return Err(format!("claude stop {short} did not answer in 20 s")),
    }
    for _ in 0..5 {
        let list = tokio::process::Command::new("claude").args(["agents", "--json", "--all"]).kill_on_drop(true)
            .stdin(std::process::Stdio::null()).output();
        let listing = match tokio::time::timeout(std::time::Duration::from_secs(20), list).await {
            Ok(Ok(out)) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            Ok(Ok(out)) => return Err(format!("claude agents failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
            Ok(Err(error)) => return Err(format!("claude agents failed: {error}")),
            Err(_) => return Err("claude agents did not answer in 20 s".into()),
        };
        if !still_running(&listing, short) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
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
pub(crate) struct Handovers {
    sessions: OnceLock<Arc<dyn BgSessions>>,
    moving: tokio::sync::Mutex<()>,
    /// A move stops right after recording this step, as a daemon that dies then does: the
    /// admission dies after its commit, before its host starts.
    #[cfg(test)]
    pub(crate) crash_after: std::sync::Mutex<Option<HandoverStep>>,
    #[cfg(test)]
    pub(crate) turn_deadline: std::sync::Mutex<Option<std::time::Duration>>,
}

/// How long a `claude --bg` node in a turn gets to end it before Alondra interrupts it to stop it.
const TURN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

impl Handovers {
    pub(crate) fn turn_deadline(&self) -> std::time::Duration {
        #[cfg(test)]
        if let Some(deadline) = *self.turn_deadline.lock().unwrap() {
            return deadline;
        }
        TURN_DEADLINE
    }

    pub(crate) fn sessions(&self) -> Arc<dyn BgSessions> {
        Arc::clone(self.sessions.get_or_init(|| Arc::new(ClaudeSessions)))
    }

    #[cfg(test)]
    pub(crate) fn use_sessions(&self, sessions: Arc<dyn BgSessions>) {
        assert!(self.sessions.set(sessions).is_ok(), "the sessions are set once");
    }
}

/// What the move to the SDK does with a task in progress, by the facts that decide it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", content = "reason", rename_all = "snake_case")]
pub(crate) enum Verdict {
    /// Its node moves: the reason says when.
    Passes(Reason),
    /// Its node stays on `claude --bg` until this fact ends.
    Waits(Reason),
    /// Nothing of it moves.
    Stays(Reason),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub(crate) enum Reason {
    NextMessage,
    MessageWaiting,
    BackFromPause,
    BackFromLostSession,
    Moving(HandoverStep),
    InTurn,
    Starting,
    BackgroundWork(String),
    AnswerInTurn,
    TurnAfterDelivery,
    MessageOnItsWay,
    DeliveryWaiting,
    AfterMerge(String),
    ThroughSdk,
    NoNode,
    Closing,
    SessionClosed,
}

/// One task in progress and what the move does with it.
#[derive(Debug, Serialize)]
pub(crate) struct Row {
    pub(crate) task: TaskId,
    pub(crate) title: String,
    pub(crate) node: Option<String>,
    #[serde(flatten)]
    pub(crate) verdict: Verdict,
}

/// The open task `agent` holds through a `claude --bg` execution.
fn bg_task<'a>(state: &'a DomainState, agent: &AgentId) -> Option<&'a Task> {
    state.tasks.values().find(|task| {
        task.agent_id.as_ref() == Some(agent)
            && !task.state.terminal()
            && !task.archived
            && state.lifecycle_of(&task.id).ok().is_some_and(|lifecycle| {
                lifecycle.execution().is_some_and(|stamp| stamp.host().as_str() == alondra_domain::CLAUDE_BG_HOST && stamp.node() == agent)
            })
    })
}

/// What the move does with the `claude --bg` node `agent` of `task`, which has no move yet.
fn judge_node(domain: &DomainRuntime, state: &DomainState, task: &Task, agent: &AgentId, message_waiting: bool) -> Verdict {
    if task.over() {
        return Verdict::Stays(Reason::Closing);
    }
    if let Some(work) = &task.wrap_up.post_merge_work {
        return Verdict::Waits(Reason::AfterMerge(work.reason.clone()));
    }
    if task.final_presentation.is_some() {
        return Verdict::Waits(Reason::DeliveryWaiting);
    }
    let Some(phase) = state.node_phase(agent) else { return Verdict::Stays(Reason::SessionClosed) };
    match phase {
        NodePhase::Paused(_) => Verdict::Passes(Reason::BackFromPause),
        NodePhase::Lost => Verdict::Passes(Reason::BackFromLostSession),
        NodePhase::WaitingUser { in_turn: false } | NodePhase::Delivered { in_turn: false } | NodePhase::WaitingDependency => {
            if domain.live.pushes.waiting(&agent.0) {
                Verdict::Waits(Reason::MessageOnItsWay)
            } else if message_waiting {
                Verdict::Passes(Reason::MessageWaiting)
            } else {
                Verdict::Passes(Reason::NextMessage)
            }
        }
        NodePhase::Working => Verdict::Waits(Reason::InTurn),
        NodePhase::Starting { .. } => Verdict::Waits(Reason::Starting),
        NodePhase::WaitingBackground(work) => Verdict::Waits(Reason::BackgroundWork(work.label.clone())),
        NodePhase::WaitingUser { in_turn: true } => Verdict::Waits(Reason::AnswerInTurn),
        NodePhase::Delivered { in_turn: true } => Verdict::Waits(Reason::TurnAfterDelivery),
        NodePhase::Resuming { .. } if message_waiting && !domain.live.pushes.waiting(&agent.0) => Verdict::Passes(Reason::MessageWaiting),
        NodePhase::Resuming { .. } => Verdict::Waits(Reason::MessageOnItsWay),
        NodePhase::Closing => Verdict::Stays(Reason::Closing),
        NodePhase::NoTask | NodePhase::Closed | NodePhase::ThroughSdk => Verdict::Stays(Reason::SessionClosed),
    }
}

/// Whether the facts let `agent`'s node move now, and whether it comes back rather than rests.
/// `message_waiting`: an outbox message for it waits, which a node resuming for it moves with.
fn movable(domain: &DomainRuntime, state: &DomainState, agent: &AgentId, message_waiting: bool) -> Option<(TaskId, bool)> {
    domain.sdk()?;
    let task = bg_task(state, agent)?;
    match judge_node(domain, state, task, agent, message_waiting) {
        Verdict::Passes(Reason::NextMessage | Reason::MessageWaiting) => Some((task.id.clone(), false)),
        Verdict::Passes(Reason::BackFromPause | Reason::BackFromLostSession) => Some((task.id.clone(), true)),
        _ => None,
    }
}

fn handover_of(domain: &DomainRuntime, agent: &AgentId) -> Option<Handover> {
    match domain.store.lock().expect("domain store poisoned").handovers() {
        Ok(handovers) => handovers.into_iter().rev().find(|handover| handover.node == *agent),
        Err(error) => {
            log!("could not read the SDK handover journal: {error:#}");
            None
        }
    }
}

/// The task and SDK node that took over `agent`'s session, once its move is done.
pub(crate) fn successor(domain: &DomainRuntime, agent: &AgentId) -> Option<(TaskId, AgentId)> {
    handover_of(domain, agent).filter(|handover| handover.step == HandoverStep::Done).and_then(|handover| Some((handover.task, handover.agent?)))
}

/// Whether `agent`'s session moved, or is moving, to the SDK: nothing restarts or removes it.
pub(crate) fn moved(domain: &DomainRuntime, agent: &AgentId) -> bool {
    handover_of(domain, agent).is_some()
}

/// Whether the outbox messages for `agent` wait for its move instead of reaching it through its
/// node mod: it is moving, or it would move with them.
pub(crate) fn holds_messages(domain: &DomainRuntime, agent: &AgentId) -> bool {
    domain.sdk().is_some()
        && (moved(domain, agent)
            || domain.state().is_ok_and(|state| movable(domain, &state, agent, true).is_some_and(|(task, _)| folder(domain, agent, &task).is_ok())))
}

/// How `tell` reached the node.
pub(crate) enum Told {
    /// Down its node mod's connection: resolved once the session takes it.
    Pushed(tokio::sync::oneshot::Receiver<()>),
    /// Through the SDK: it opens the node's session there, or follows it to the SDK node.
    Moving,
}

/// Tells the `claude --bg` node `session` `text` from `from`: down its connection as before, or,
/// once the SDK backend is on and it rests or comes back, as the input that moves it to the SDK.
pub(crate) fn tell(domain: &DomainRuntime, session: &str, from: &str, text: &str, as_user: bool) -> Result<Told, Unreachable> {
    let agent = AgentId(session.to_owned());
    if domain.sdk().is_some() {
        let stamped = crate::node_push::stamped(from, alondra_core::now_millis(), text);
        if let Some(handover) = handover_of(domain, &agent) {
            follow(domain, handover, stamped);
            return Ok(Told::Moving);
        }
        if let Some((task, returning)) = domain.state().ok().and_then(|state| movable(domain, &state, &agent, false)) {
            match plan(domain, &agent, &task, stamped, None, returning) {
                Ok(true) => return Ok(Told::Moving),
                Ok(false) => {}
                Err(error) => log!("{session} did not move to the Agent SDK and is told through its connection: {error:#}"),
            }
        }
    }
    domain.live.pushes.message(session, from, text, as_user).map(Told::Pushed)
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
                () = tokio::time::sleep(EVERY) => {}
            }
        }
    });
}

/// Records the move of `agent`, whose input is `input`, and runs it in the background. False when
/// the node already has a move or the SDK backend cannot take it.
pub(crate) fn plan(domain: &DomainRuntime, agent: &AgentId, task: &TaskId, input: String, outbox: Option<i64>, returning: bool) -> anyhow::Result<bool> {
    let Some((_, runtime)) = domain.sdk() else { return Ok(false) };
    if !record(domain, agent, task, input, outbox, returning)? {
        return Ok(false);
    }
    let node = agent.clone();
    tokio::spawn(async move {
        if let Err(why) = advance(&runtime, &node).await {
            log!("the move of {node} to the Agent SDK stopped and goes on later: {why}");
        }
    });
    Ok(true)
}

/// Records the plan of `agent`'s move: the session, its folder, the process that runs it and the
/// input that opens it through the SDK.
fn record(domain: &DomainRuntime, agent: &AgentId, task: &TaskId, input: String, outbox: Option<i64>, returning: bool) -> anyhow::Result<bool> {
    let cwd = folder(domain, agent, task)?;
    let state = domain.state()?;
    let name = state.agents.get(agent).map(|known| known.name.clone()).unwrap_or_else(|| agent.0.clone());
    let process = domain.handovers.sessions().seen(domain, &agent.0);
    let plan = HandoverPlan { session: agent.0.clone(), cwd, name, process, input, outbox, returning };
    if !domain.store.lock().expect("domain store poisoned").plan_handover(agent, task, &plan)? {
        return Ok(false);
    }
    log!("{agent} ({task}) moves to the Agent SDK{}", if returning { " as it comes back" } else { " with its next input" });
    Ok(true)
}

/// The folder `agent`'s session works in, where the SDK resumes it: the worktree Claude Code made
/// for it, else the one it reported, else its task's. An error says why the node cannot move.
fn folder(domain: &DomainRuntime, agent: &AgentId, task: &TaskId) -> anyhow::Result<String> {
    anyhow::ensure!(crate::plugins::node_in(domain.home()).is_some(), "Alondra's node plugin, which the SDK node loads, was not found");
    crate::worktree::worktree_of(&agent.0).and_then(|dir| dir.to_str().map(str::to_owned))
        .or_else(|| domain.live.mods.session(&agent.0).and_then(|reported| reported.cwd))
        .or_else(|| domain.task_worktree(task).ok().flatten())
        .ok_or_else(|| anyhow::anyhow!("the folder its session works in is not known"))
}

/// Takes `agent`'s move as far as the facts let it: from the step recorded to the end, or to a
/// step that waits for a fact (the node at rest again). An error leaves the move at its step.
pub(crate) async fn advance(domain: &Arc<DomainRuntime>, agent: &AgentId) -> Result<HandoverStep, String> {
    let _one = domain.handovers.moving.lock().await;
    loop {
        let handover = handover_of(domain, agent).ok_or_else(|| format!("{agent} has no move"))?;
        let step = handover.step;
        #[cfg(test)]
        if *domain.handovers.crash_after.lock().unwrap() == Some(step) && step != HandoverStep::SdkAdmitted {
            return Err(format!("the daemon died after {step:?}"));
        }
        match step {
            HandoverStep::Planned => {
                if !stop(domain, &handover).await? {
                    return Ok(step);
                }
            }
            HandoverStep::BgStopped => admit(domain, &handover).await?,
            HandoverStep::SdkAdmitted => finish(domain, &handover).await?,
            HandoverStep::Done => return Ok(step),
        }
    }
}

/// Stops the `claude --bg` process of the move and records its exit, seen by its identity: the
/// process seen at the plan and any Claude Code lists now must both be gone. False while a node
/// that rests is at work again: the move waits for its next rest.
async fn stop(domain: &Arc<DomainRuntime>, handover: &Handover) -> Result<bool, String> {
    let node = &handover.node;
    if !handover.plan.returning {
        let state = domain.state().map_err(|error| format!("{error:#}"))?;
        if !bg_task(&state, node).is_some_and(|task| matches!(judge_node(domain, &state, task, node, true), Verdict::Passes(_))) {
            return Ok(false);
        }
    }
    let sessions = domain.handovers.sessions();
    let mut processes: Vec<HandoverProcess> = handover.plan.process.into_iter().collect();
    processes.extend(sessions.listed(&node.0).await?.filter(|listed| !processes.contains(listed)));
    if processes.iter().any(|process| sessions.alive(process)) {
        sessions.stop(&node.0).await?;
    }
    if let Some(running) = processes.iter().find(|process| sessions.alive(process)) {
        return Err(format!("the claude --bg process of {node} (pid {}) still runs after it was stopped", running.pid));
    }
    if let Some(listed) = sessions.listed(&node.0).await?.filter(|listed| sessions.alive(listed)) {
        return Err(format!("Claude Code still runs {node} as pid {}", listed.pid));
    }
    domain.store.lock().expect("domain store poisoned").record_bg_stopped(node, &handover.task, processes.first().copied())
        .map_err(|error| format!("recording that the claude --bg process stopped: {error:#}"))?;
    Ok(true)
}

/// Admits the SDK execution that resumes the node's session with the move's input, and records the
/// admission in the same transaction; its owner starts at once.
async fn admit(domain: &Arc<DomainRuntime>, handover: &Handover) -> Result<(), String> {
    let (backend, runtime) = domain.sdk().ok_or("this daemon has no SDK backend")?;
    let backend = backend.clone();
    let plugin = crate::plugins::node_in(domain.home()).ok_or("Alondra's node plugin, which the SDK node loads, was not found")?;
    let state = domain.state().map_err(|error| format!("{error:#}"))?;
    let task = state.tasks.get(&handover.task).ok_or("the task is gone")?;
    let options = crate::sdk_owner::LaunchOptions {
        provider: alondra_core::accounts::Provider::Claude,
        selection: None,
        model: task.model.as_ref().map(|model| model.model.clone()),
        effort: None,
        disallowed_tools: domain.schedule_request_tools(&task.request_id).ok().flatten().unwrap_or_default(),
    };
    let launch = crate::sdk_owner::Launch {
        task: handover.task.clone(),
        name: handover.plan.name.clone(),
        directory: handover.plan.cwd.clone(),
        prompt: handover.plan.input.clone(),
        plugin,
        options,
        model: None,
        account: alondra_core::accounts::AccountId::default_account(),
    };
    let continuation = crate::sdk_owner::Continuation::Node(handover.node.clone());
    let driver = tokio::task::spawn_blocking(move || crate::sdk_owner::continue_session(runtime, &backend, launch, continuation))
        .await
        .map_err(|error| format!("the admission stopped: {error}"))?
        .map_err(|crate::sdk_owner::Failed { why, .. }| format!("the SDK execution was not admitted: {why}"))?;
    domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));
    domain.live.forget_closed_task(&handover.task.0);
    Ok(())
}

/// Makes sure the SDK node runs the task, hands the outbox message the move carried, closes the
/// old node and records the end of the move.
async fn finish(domain: &Arc<DomainRuntime>, handover: &Handover) -> Result<(), String> {
    let agent = handover.agent.clone().ok_or("the admission names no SDK node")?;
    if !domain.owns_sdk_host(&agent) {
        let (backend, runtime) = domain.sdk().ok_or("this daemon has no SDK backend")?;
        let backend = backend.clone();
        let plugin = crate::plugins::node_in(domain.home()).ok_or("Alondra's node plugin, which the SDK node loads, was not found")?;
        let task = handover.task.clone();
        let driver = tokio::task::spawn_blocking(move || {
            if crate::sdk_owner::host_started(&backend, &runtime, &task) {
                crate::sdk_owner::adopt(runtime, &backend, task, Some(&plugin))
            } else {
                crate::sdk_owner::launch_admitted(runtime, &backend, task, &plugin)
            }
        })
        .await
        .map_err(|error| format!("starting the SDK node stopped: {error}"))?
        .map_err(|crate::sdk_owner::Failed { why, .. }| format!("the SDK node did not start: {why}"))?;
        domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));
    }
    let node = &handover.node;
    let mut store = domain.store.lock().expect("domain store poisoned");
    if let Some(id) = handover.plan.outbox {
        store.hand_carried_message(id).map_err(|error| format!("handing the message the move carried: {error:#}"))?;
    }
    drop(store);
    if let Err(failure) = crate::domain_agent_lifecycle::node_event(domain, node, NodeEvent::SessionClosed) {
        log!("could not close {node} after its move to the Agent SDK: {failure}");
    }
    crate::domain_transport::session_ended(domain, node).map_err(|error| format!("ending the old node's session: {error:#}"))?;
    domain.live.mark_closed(&node.0);
    domain.live.apply(&alondra_core::Event::NodeRemoved { session_id: node.0.clone() });
    domain.forget_ended_session(&node.0).map_err(|error| format!("{error:#}"))?;
    domain.store.lock().expect("domain store poisoned").record_handover_done(node, &handover.task)
        .map_err(|error| format!("recording the end of the move: {error:#}"))?;
    log!("{node} ({}) moved to the Agent SDK: {agent} resumes its Claude session", handover.task);
    Ok(())
}

/// Every task in progress and what the move does with it, without doing anything.
pub(crate) fn dry_run(domain: &DomainRuntime) -> anyhow::Result<Vec<Row>> {
    let state = domain.state()?;
    let (handovers, pending) = {
        let store = domain.store.lock().expect("domain store poisoned");
        (store.handovers()?, store.pending_messages()?)
    };
    let mut rows = vec![];
    for task in state.tasks.values().filter(|task| !task.state.terminal() && !task.archived) {
        let node = task.agent_id.as_ref();
        let moving = node.and_then(|agent| handovers.iter().rev().find(|handover| handover.node == *agent && handover.step != HandoverStep::Done));
        let verdict = match (node, moving) {
            (None, _) => Verdict::Stays(Reason::NoNode),
            (Some(_), Some(handover)) => Verdict::Passes(Reason::Moving(handover.step)),
            (Some(agent), None) if bg_task(&state, agent).is_some() => {
                let waiting = pending.iter().any(|message| message.recipient_kind == "agent" && message.agent_id.as_deref() == Some(agent.0.as_str()));
                judge_node(domain, &state, task, agent, waiting)
            }
            (Some(agent), None) if crate::sdk_owner::runs_through_sdk(&state, agent) => Verdict::Stays(Reason::ThroughSdk),
            (Some(_), None) => Verdict::Stays(Reason::NoNode),
        };
        let name = node.map(|agent| state.agents.get(agent).map_or_else(|| agent.0.clone(), |known| known.name.clone()));
        rows.push(Row { task: task.id.clone(), title: task.title.clone(), node: name, verdict });
    }
    Ok(rows)
}

/// Goes on with every move a previous daemon left, then, while the daemon runs, moves each node at
/// rest that an outbox message waits for, with that message, and goes on with the moves that
/// waited. Runs only with an SDK backend.
pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    if domain.sdk().is_none() {
        return;
    }
    let mut changed = domain.subscribe();
    loop {
        tick(&domain).await;
        tokio::select! {
            _ = changed.changed() => {}
            () = tokio::time::sleep(EVERY) => {}
        }
    }
}

async fn tick(domain: &Arc<DomainRuntime>) {
    plan_waiting(domain);
    let open: Vec<AgentId> = domain.store.lock().expect("domain store poisoned").handovers().unwrap_or_default().into_iter()
        .filter(|handover| handover.step != HandoverStep::Done).map(|handover| handover.node).collect();
    for node in open {
        if let Err(why) = advance(domain, &node).await {
            log!("the move of {node} to the Agent SDK waits: {why}");
        }
    }
}

/// Plans the move of each node at rest an outbox message waits for, with that message.
fn plan_waiting(domain: &DomainRuntime) {
    let pending = domain.store.lock().expect("domain store poisoned").pending_messages().unwrap_or_default();
    let Ok(state) = domain.state() else { return };
    let mut planned = std::collections::BTreeSet::new();
    for message in pending.iter().filter(|message| message.recipient_kind == "agent") {
        let Some(agent) = message.agent_id.as_deref().map(|agent| AgentId(agent.to_owned())) else { continue };
        if planned.contains(&agent) || moved(domain, &agent) {
            continue;
        }
        let Some((task, returning)) = movable(domain, &state, &agent, true) else { continue };
        let text = match crate::domain_transport::node_text(domain, &state, message) {
            Ok(text) => text,
            Err(error) => {
                log!("could not read the message for {agent} that would move it to the Agent SDK: {error:#}");
                continue;
            }
        };
        match record(domain, &agent, &task, text, Some(message.id), returning) {
            Ok(_) => {
                planned.insert(agent);
            }
            Err(error) => log!("{agent} did not move to the Agent SDK with the message waiting for it: {error:#}"),
        }
    }
}

#[cfg(test)]
#[path = "sdk_handover/tests.rs"]
mod tests;
