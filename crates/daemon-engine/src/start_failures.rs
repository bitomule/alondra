//! What keeps every open task in someone's hands once its start goes wrong: the failures are
//! recorded in the kernel (`alondra_engine_kernel::start_failures`); the engine adds what it knows
//! of how each open task gets going.
use crate::gate::GateOf as _;
use crate::live::LiveOf as _;
use crate::locks::Poisonable as _;
use crate::live::Relaunch;
use alondra_engine_ports::Runtime;
use crate::domain_store::{AutoLaunchReason, AutoLaunchState};
use alondra_domain_rules::{DomainState, NodeFact, Start, StartFacts, TaskId};
use anyhow::Result;
use std::collections::BTreeMap;
pub use alondra_engine_kernel::start_failures::{ended, failed, plan_changed, problem_of, raise};

/// What the daemon knows of how every open task gets going.
pub fn facts(domain: &dyn Runtime, state: &DomainState) -> Result<BTreeMap<TaskId, StartFacts>> {
    let held = domain.gate().pauses().held;
    let store = domain.store().lock_or_exit("the domain store");
    let planned = store.planned_tasks()?;
    let autos = store.auto_launches()?;
    let retries = store.start_retries()?;
    let queued: Vec<TaskId> = store.launch_queue()?.into_iter().map(|row| row.task_id).collect();
    let now = alondra_core::now_millis();
    let mut out = BTreeMap::new();
    for task in state.tasks.values().filter(|task| !task.over() && !task.archived) {
        let scheduled = state.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(&task.id));
        let auto = autos.iter().find(|row| row.task_id == task.id);
        let closed = task.released.as_ref().map(|node| node.relaunch);
        let relaunching = closed == Some(Relaunch::ByItself);
        let start = match (retries.get(&task.id).filter(|(_, at)| *at > now), auto.map(|row| (row.state, row.reason))) {
            (Some((attempt, at)), _) => Start::Retry { attempt: *attempt, at: *at },
            (None, Some((AutoLaunchState::Blocked, Some(AutoLaunchReason::UsageExhausted)))) => Start::Ready,
            (None, Some((AutoLaunchState::Blocked, _))) => Start::Blocked,
            (None, Some((AutoLaunchState::Launching, _))) => Start::Launching,
            (None, Some((AutoLaunchState::Launched, _))) if relaunching => Start::Ready,
            (None, Some((AutoLaunchState::Launched, _))) => Start::Launched,
            (None, None) if domain.live().is_launching(&task.id.0) || queued.contains(&task.id) => Start::Launching,
            (None, None) if !relaunching && store.task_was_launched(&task.id)? => Start::Launched,
            (None, None) => Start::Ready,
        };
        let node = match task.agent_id.as_ref().and_then(|id| state.agents.get(id)) {
            Some(agent) if agent.phase.live() => crate::domain_transport::stopped_without_delivering(domain, state, &agent.id,
                |task| store.stop_blocks(task).ok().flatten()).map_or(NodeFact::Alive, |sent_back| NodeFact::Stopped { sent_back }),
            Some(_) | None => NodeFact::Gone,
        };
        out.insert(task.id.clone(), StartFacts {
            planned: scheduled || planned.contains_key(&task.id),
            start,
            node,
            held: held.contains_key(&task.id),
            closed: closed == Some(Relaunch::AskUser),
        });
    }
    Ok(out)
}


