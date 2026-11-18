//! What keeps every open task in someone's hands once its start goes wrong: a failed start is
//! tried again by itself a few times and then put in front of the user, and a node the user closed
//! asks whether to relaunch. The decisions are the domain's (`after_failure`, `DomainState::react`);
//! this carries them out.
use crate::gate::GateOf as _;
use crate::live::LiveOf as _;
use crate::locks::Poisonable as _;
use crate::live::Relaunch;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{AutoLaunchReason, AutoLaunchState};
use alondra_domain_rules::{
    AfterFailure, Command, CommandEnvelope, DomainState, NodeFact, Start, StartFacts, StartProblem, TaskId,
    PROTOCOL_VERSION, after_failure,
};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

/// A start of the task failed: Alondra tries again later, or asks the user.
pub fn failed(domain: &DomainRuntime, task: &TaskId, problem: StartProblem, now: i64) -> Result<()> {
    if ended(domain, task)? {
        return Ok(());
    }
    let attempts = domain.store.lock_or_exit("the domain store").start_retries()?.get(task).map_or(0, |(attempts, _)| *attempts);
    match after_failure(attempts, problem, now) {
        AfterFailure::Retry { attempt, at } => {
            let mut store = domain.store.lock_or_exit("the domain store");
            store.set_start_retry(task, Some((attempt, at)))?;
            store.forget_auto_launch(task)?;
            log!("the start of {task} failed; Alondra tries again by itself (attempt {attempt})");
        }
        AfterFailure::Ask(problem) => {
            {
                let mut store = domain.store.lock_or_exit("the domain store");
                store.set_start_retry(task, None)?;
                let (reason, detail) = match &problem {
                    StartProblem::Untrusted { folder, .. } => (AutoLaunchReason::WorkspaceUntrusted, Some(folder.as_str())),
                    StartProblem::LaunchFailed { error } | StartProblem::PlanRefused { error } => (AutoLaunchReason::LaunchFailed, Some(error.as_str())),
                    StartProblem::NodeClosed | StartProblem::DependencyCancelled { .. } | StartProblem::ApiFailed { .. } | StartProblem::MoveStartRefused { .. } => (AutoLaunchReason::LaunchFailed, None),
                };
                store.set_auto_launch(task, AutoLaunchState::Blocked, Some(reason), detail, now)?;
            }
            raise(domain, task, problem)?;
        }
    }
    Ok(())
}

/// The plan a start was refused for changed: the orchestrator's card about it no longer holds, and
/// the task goes back to the gate to start with the new plan.
pub fn plan_changed(domain: &DomainRuntime, task: &TaskId) -> Result<()> {
    let state = domain.state()?;
    let Some(card) = state.open_plan_refusal(task) else { return Ok(()) };
    domain.withdraw_problem(&card.id).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    {
        let mut store = domain.store.lock_or_exit("the domain store");
        store.forget_auto_launch(task)?;
        store.set_start_retry(task, None)?;
        store.set_launch_refusal(task, None, alondra_core::now_millis())?;
    }
    domain.gate().poke();
    Ok(())
}

/// Whether the task ended (cancelled, done or archived) while it was starting: then its start did
/// not fail, it no longer waits. It leaves without a retry or a card, and its slot at the gate goes
/// to the next one at once.
pub fn ended(domain: &DomainRuntime, task: &TaskId) -> Result<bool> {
    let state = domain.state()?;
    if state.tasks.get(task).is_some_and(|task| !task.over() && !task.archived) {
        return Ok(false);
    }
    {
        let mut store = domain.store.lock_or_exit("the domain store");
        store.set_start_retry(task, None)?;
        store.forget_auto_launch(task)?;
        store.dequeue_launch(task)?;
    }
    let key = alondra_engine_ports::admission::Key::Task(task.clone());
    domain.gate().forget(&key);
    domain.gate().not_started(&key);
    domain.gate().poke();
    Ok(true)
}

/// The problem a refused launch is, by the code it was refused with; none for refusals that clear
/// by themselves (usage, disk, a launch already under way). A refusal of the task's own plan or
/// choices is the same on every try.
pub fn problem_of(code: &str, message: &str, cwd: Option<&str>) -> Option<StartProblem> {
    match code {
        "usage_exhausted" | "worktrees_unavailable" | "disk_low" | "conflict" | alondra_domain_rules::capability_capacity_card::CAPABILITY_CAPACITY_CODE => None,
        "invalid_input" | "model_is_binding" | "unsupported_capability" | "selection_transition_required" | "active_account_immutable" =>
            Some(StartProblem::PlanRefused { error: message.trim().to_owned() }),
        "workspace_untrusted" => {
            let folder = cwd.map(|cwd| crate::claude_trust::check(Path::new(cwd)).map_or_else(|| cwd.to_owned(), |repo| repo.to_string_lossy().into_owned()));
            Some(StartProblem::Untrusted { folder: folder.unwrap_or_else(|| message.to_owned()), error: None })
        }
        _ => Some(StartProblem::LaunchFailed { error: message.trim().to_owned() }),
    }
}

pub fn raise(domain: &DomainRuntime, task: &TaskId, problem: StartProblem) -> Result<()> {
    let state = domain.state()?;
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("start-problem-{}-{}", task.0, crate::auth::random_hex(8)).into(), expected_revision: state.revision,
        command: Command::RaiseStartProblem { task_id: task.clone(), problem },
    };
    domain.execute_as_system(envelope).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(())
}

/// What the daemon knows of how every open task gets going.
pub fn facts(domain: &DomainRuntime, state: &DomainState) -> Result<BTreeMap<TaskId, StartFacts>> {
    let held = domain.gate().pauses().held;
    let store = domain.store.lock_or_exit("the domain store");
    let planned = store.planned_tasks()?;
    let autos = store.auto_launches()?;
    let retries = store.start_retries()?;
    let queued: Vec<TaskId> = store.launch_queue()?.into_iter().map(|row| row.task_id).collect();
    let now = alondra_core::now_millis();
    let mut out = BTreeMap::new();
    for task in state.tasks.values().filter(|task| !task.over() && !task.archived) {
        let scheduled = state.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(&task.id));
        let auto = autos.iter().find(|row| row.task_id == task.id);
        let closed = domain.live().closed_task(&task.id.0).map(|node| node.relaunch);
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


