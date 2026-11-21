//! The moves of `claude --bg` nodes to the Agent SDK that an earlier release planned: the same Claude
//! session, in the same folder, resumed through the SDK. No node runs on `claude --bg` any more, so
//! no new move is planned; a move already journaled goes on to its end.
//!
//! Each move is journaled in domain.db: planned with its input, the `claude --bg` process stopped
//! and its exit seen by the process's identity, the SDK execution admitted in the transaction
//! that records it, then the old node closed. A daemon that comes back goes on from the last step
//! recorded, so no step's effect happens twice, and the task never has two live owners.
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use crate::sdk_owner::SdkOwners as _;
use crate::engine::EngineOf as _;
use alondra_engine_kernel::bg_sessions::{EVERY, HandoversOf as _, handover_of};
use alondra_core::clock;
use crate::locks::Poisonable as _;
use alondra_engine_ports::Runtime;
use crate::domain_store::{Handover, HandoverProcess, HandoverStep};
use alondra_domain_rules::*;
use serde::Serialize;
use std::sync::Arc;

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
    Moving(HandoverStep),
    DeliveryWaiting,
    AfterMerge(String),
    ThroughSdk,
    NoNode,
    Closing,
    SessionClosed,
}

/// One task in progress and what the move does with it.
#[derive(Debug, Serialize)]
pub struct Row {
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

/// What the move does with the `claude --bg` node `agent` of `task`, which has no move yet. No node
/// runs on `claude --bg` any more: whatever its task waits for, its session is closed.
fn judge_node(task: &Task) -> Verdict {
    if task.over() {
        return Verdict::Stays(Reason::Closing);
    }
    if let Some(work) = &task.wrap_up.post_merge_work {
        return Verdict::Waits(Reason::AfterMerge(work.reason.clone()));
    }
    if task.final_presentation.is_some() {
        return Verdict::Waits(Reason::DeliveryWaiting);
    }
    Verdict::Stays(Reason::SessionClosed)
}

/// Takes `agent`'s move as far as the facts let it: from the step recorded to the end, or to a
/// step that waits for a fact (the node at rest again). An error leaves the move at its step.
pub(crate) async fn advance(domain: &Arc<dyn Runtime>, agent: &AgentId) -> Result<HandoverStep, String> {
    let _one = domain.handovers().moving.lock().await;
    loop {
        let handover = handover_of(&**domain, agent).ok_or_else(|| format!("{agent} has no move"))?;
        let step = handover.step;
        if *domain.handovers().crash_after.lock().unwrap() == Some(step) && step != HandoverStep::SdkAdmitted {
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
/// process seen at the plan and any Claude Code lists now must both be gone.
async fn stop(domain: &Arc<dyn Runtime>, handover: &Handover) -> Result<bool, String> {
    let node = &handover.node;
    let sessions = domain.handovers().sessions();
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
    domain.store().lock_or_exit("the domain store").record_bg_stopped(node, &handover.task, processes.first().copied())
        .map_err(|error| format!("recording that the claude --bg process stopped: {error:#}"))?;
    Ok(true)
}

/// Admits the SDK execution that resumes the node's session with the move's input, and records the
/// admission in the same transaction; its owner starts at once.
async fn admit(domain: &Arc<dyn Runtime>, handover: &Handover) -> Result<(), String> {
    let (backend, runtime) = domain.sdk().ok_or("this daemon has no SDK backend")?;
    let backend = backend.clone();
    let plugin = domain.ports().plugins().node(domain.home()).ok_or("Alondra's node plugin, which the SDK node loads, was not found")?;
    let state = domain.state();
    let task = state.tasks.get(&handover.task).ok_or("the task is gone")?;
    let options = alondra_daemon_engine_model::LaunchOptions {
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
    domain.engine().forget_closed_task(&handover.task);
    Ok(())
}

/// Makes sure the SDK node runs the task, hands the outbox message the move carried, closes the
/// old node and records the end of the move.
async fn finish(domain: &Arc<dyn Runtime>, handover: &Handover) -> Result<(), String> {
    let agent = handover.agent.clone().ok_or("the admission names no SDK node")?;
    if !domain.owns_sdk_host(&agent) {
        let (backend, runtime) = domain.sdk().ok_or("this daemon has no SDK backend")?;
        let backend = backend.clone();
        let plugin = domain.ports().plugins().node(domain.home()).ok_or("Alondra's node plugin, which the SDK node loads, was not found")?;
        let task = handover.task.clone();
        let driver = tokio::task::spawn_blocking(move || {
            if crate::sdk_owner::host_started(&backend, &*runtime, &task) {
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
    let mut store = domain.store().lock_or_exit("the domain store");
    if let Some(id) = handover.plan.outbox {
        store.hand_carried_message(id).map_err(|error| format!("handing the message the move carried: {error:#}"))?;
    }
    drop(store);
    if let Err(failure) = domain.engine().node_event(node, NodeEvent::SessionClosed) {
        log!("could not close {node} after its move to the Agent SDK: {failure}");
    }
    domain.engine().session_ended(node).map_err(|error| format!("ending the old node's session: {error:#}"))?;
    domain.engine().node_removed(&node.0);
    domain.engine().forget_ended_session(&node.0).map_err(|error| format!("{error:#}"))?;
    domain.store().lock_or_exit("the domain store").record_handover_done(node, &handover.task)
        .map_err(|error| format!("recording the end of the move: {error:#}"))?;
    log!("{node} ({}) moved to the Agent SDK: {agent} resumes its Claude session", handover.task);
    Ok(())
}

/// Every task in progress and what the move does with it, without doing anything.
pub fn dry_run(domain: &dyn Runtime) -> anyhow::Result<Vec<Row>> {
    let state = domain.state();
    let handovers = domain.store().lock_or_exit("the domain store").handovers()?;
    let mut rows = vec![];
    for task in state.tasks.values().filter(|task| !task.state.terminal() && !task.archived) {
        let node = task.agent_id.as_ref();
        let moving = node.and_then(|agent| handovers.iter().rev().find(|handover| handover.node == *agent && handover.step != HandoverStep::Done));
        let verdict = match (node, moving) {
            (None, _) => Verdict::Stays(Reason::NoNode),
            (Some(_), Some(handover)) => Verdict::Passes(Reason::Moving(handover.step)),
            (Some(agent), None) if bg_task(&state, agent).is_some() => judge_node(task),
            (Some(agent), None) if crate::sdk_owner::runs_through_sdk(&state, agent) => Verdict::Stays(Reason::ThroughSdk),
            (Some(_), None) => Verdict::Stays(Reason::NoNode),
        };
        let name = node.map(|agent| state.agents.get(agent).map_or_else(|| agent.0.clone(), |known| known.name.clone()));
        rows.push(Row { task: task.id.clone(), title: task.title.clone(), node: name, verdict });
    }
    Ok(rows)
}

/// Goes on with every move a previous daemon left. Runs only with an SDK backend.
pub async fn run(domain: Arc<dyn Runtime>) {
    if domain.sdk().is_none() {
        return;
    }
    let mut changed = domain.subscribe();
    loop {
        tick(&domain).await;
        tokio::select! {
            _ = changed.changed() => {}
            () = clock::sleep(EVERY) => {}
        }
    }
}

async fn tick(domain: &Arc<dyn Runtime>) {
    let open: Vec<AgentId> = domain.store().lock_or_exit("the domain store").handovers().unwrap_or_default().into_iter()
        .filter(|handover| handover.step != HandoverStep::Done).map(|handover| handover.node).collect();
    for node in open {
        if let Err(why) = advance(domain, &node).await {
            log!("the move of {node} to the Agent SDK waits: {why}");
        }
    }
}

#[cfg(all(test, feature = "real-hosts"))]
#[path = "bg_handover/tests.rs"]
mod tests;
