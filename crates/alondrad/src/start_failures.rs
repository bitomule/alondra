//! What keeps every open task in someone's hands once its start goes wrong: a failed start is
//! tried again by itself a few times and then put in front of the user, a node the user closed
//! asks whether to relaunch, a cancelled dependency asks whether to start without it. The
//! decisions are the domain's (`after_failure`, `DomainState::react`); this carries them out.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{AutoLaunchReason, AutoLaunchState};
use alondra_core::domain::{NodePhase, 
    AfterFailure, Command, CommandEnvelope, DomainState, NodeFact, Reaction, Start, StartFacts, StartProblem, TaskId,
    PROTOCOL_VERSION, after_failure,
};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

/// A start of the task failed: Alondra tries again later, or asks the user.
pub(crate) fn failed(domain: &DomainRuntime, task: &TaskId, problem: StartProblem, now: i64) -> Result<()> {
    let attempts = domain.store.lock().expect("domain store poisoned").start_retries()?.get(task).map_or(0, |(attempts, _)| *attempts);
    match after_failure(attempts, problem, now) {
        AfterFailure::Retry { attempt, at } => {
            let mut store = domain.store.lock().expect("domain store poisoned");
            store.set_start_retry(task, Some((attempt, at)))?;
            store.forget_auto_launch(task)?;
            log!("the start of {task} failed; Alondra tries again by itself (attempt {attempt})");
        }
        AfterFailure::Ask(problem) => {
            {
                let mut store = domain.store.lock().expect("domain store poisoned");
                store.set_start_retry(task, None)?;
                let (reason, detail) = match &problem {
                    StartProblem::Untrusted { folder, .. } => (AutoLaunchReason::WorkspaceUntrusted, Some(folder.as_str())),
                    StartProblem::LaunchFailed { error } => (AutoLaunchReason::LaunchFailed, Some(error.as_str())),
                    StartProblem::NodeClosed | StartProblem::DependencyCancelled { .. } | StartProblem::ApiFailed { .. } => (AutoLaunchReason::LaunchFailed, None),
                };
                store.set_auto_launch(task, AutoLaunchState::Blocked, Some(reason), detail, now)?;
            }
            raise(domain, task, problem)?;
        }
    }
    Ok(())
}

/// The problem a refused launch is, by the code it was refused with; none for refusals that clear
/// by themselves (usage, disk, a launch already under way).
pub(crate) fn problem_of(code: &str, message: &str, cwd: Option<&str>) -> Option<StartProblem> {
    match code {
        "usage_exhausted" | "disk_low" | "conflict" => None,
        "workspace_untrusted" => {
            let folder = cwd.map(|cwd| crate::claude_trust::check(Path::new(cwd)).map_or_else(|| cwd.to_owned(), |repo| repo.to_string_lossy().into_owned()));
            Some(StartProblem::Untrusted { folder: folder.unwrap_or_else(|| message.to_owned()), error: None })
        }
        _ => Some(StartProblem::LaunchFailed { error: message.trim().to_owned() }),
    }
}

pub(crate) fn raise(domain: &DomainRuntime, task: &TaskId, problem: StartProblem) -> Result<()> {
    let state = domain.state()?;
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("start-problem-{}-{}", task.0, crate::auth::random_hex(8)).into(), expected_revision: state.revision,
        command: Command::RaiseStartProblem { task_id: task.clone(), problem },
    };
    domain.execute(&alondra_core::domain::Actor::System, &envelope, None, || Ok(())).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(())
}

/// What the daemon knows of how every open task gets going.
pub(crate) fn facts(domain: &DomainRuntime, state: &DomainState) -> Result<BTreeMap<TaskId, StartFacts>> {
    let store = domain.store.lock().expect("domain store poisoned");
    let planned = store.planned_tasks()?;
    let autos = store.auto_launches()?;
    let retries = store.start_retries()?;
    let queued: Vec<TaskId> = store.launch_queue()?.into_iter().map(|row| row.task_id).collect();
    let held = domain.live.gate.pauses(domain).held;
    let now = alondra_core::now_millis();
    let mut out = BTreeMap::new();
    for task in state.tasks.values().filter(|task| !task.state.terminal() && !task.archived) {
        let scheduled = state.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(&task.id));
        let auto = autos.iter().find(|row| row.task_id == task.id);
        let start = match (retries.get(&task.id), auto.map(|row| (row.state, row.reason))) {
            (Some((attempt, at)), _) => Start::Retry { attempt: *attempt, at: *at },
            (None, Some((AutoLaunchState::Blocked, Some(AutoLaunchReason::UsageExhausted)))) => Start::Ready,
            (None, Some((AutoLaunchState::Blocked, _))) => Start::Blocked,
            (None, Some((AutoLaunchState::Launching, _))) => Start::Launching,
            (None, Some((AutoLaunchState::Launched, _))) => Start::Launched,
            (None, None) if domain.live.is_launching(&task.id.0) || queued.contains(&task.id) => Start::Launching,
            (None, None) if store.task_was_launched(&task.id)? => Start::Launched,
            (None, None) => Start::Ready,
        };
        let node = match task.agent_id.as_ref().and_then(|id| state.agents.get(id)) {
            Some(agent) if agent.phase == NodePhase::Starting { reviving: true } => match crate::domain_holds::waits_on_user(state, &agent.id.0) {
                true => NodeFact::Asking,
                false => NodeFact::Reviving,
            },
            Some(agent) if agent.phase.live() => crate::domain_transport::stopped_without_delivering(domain, state, &agent.id, now,
                |task| store.stop_blocks(task).ok().flatten()).map_or(NodeFact::Alive, |sent_back| NodeFact::Stopped { sent_back }),
            Some(_) | None => NodeFact::Gone,
        };
        out.insert(task.id.clone(), StartFacts {
            planned: scheduled || planned.contains_key(&task.id),
            start,
            node,
            held: held.contains_key(&task.id),
            closed: domain.live.closed_task(&task.id.0).is_some(),
        });
    }
    Ok(out)
}

/// Carries out what must happen now so no open task is left in nobody's hands.
pub(crate) async fn react(domain: &DomainRuntime, home: &Path, launcher: &impl crate::orchestrate::Launcher) -> Result<()> {
    crate::api_failures::settle_cards(domain)?;
    let state = domain.state()?;
    let facts = facts(domain, &state)?;
    for reaction in state.react(&facts) {
        match reaction {
            Reaction::Ask(task, problem) => {
                raise(domain, &task, problem)?;
            }
            Reaction::Retry(task) => retry(domain, home, &task, facts.get(&task).is_some_and(|f| f.closed), launcher).await?,
        }
    }
    Ok(())
}

/// The user asked to try again: a task whose node they closed gets a new node in that node's
/// folder; any other goes back to the gate, which starts it with its plan. A relaunch refused
/// for a reason of its own is a failed start like any other. Trying again a folder Claude Code
/// did not trust is the user trusting it, so Alondra records that first, or says why it could not.
async fn retry(domain: &DomainRuntime, home: &Path, task: &TaskId, closed: bool, launcher: &impl crate::orchestrate::Launcher) -> Result<()> {
    let untrusted = domain.state()?.last_start_problem(task).and_then(|problem| serde_json::from_str::<StartProblem>(&problem.body).ok());
    if let Some(StartProblem::Untrusted { folder, .. }) = untrusted
        && let Err(error) = crate::claude_trust::grant(Path::new(&folder))
    {
        log!("could not trust {folder} in Claude Code for {task}: {error}");
        return failed(domain, task, StartProblem::Untrusted { folder, error: Some(error) }, alondra_core::now_millis());
    }
    {
        let mut store = domain.store.lock().expect("domain store poisoned");
        store.forget_auto_launch(task)?;
        store.set_start_retry(task, None)?;
        store.set_launch_refusal(task, None, alondra_core::now_millis())?;
    }
    if !closed {
        return Ok(());
    }
    let actor = alondra_core::domain::Actor::System;
    match crate::domain_orchestrator_tools::relaunch(domain, home, &actor, task, launcher).await {
        Err(error) if !matches!(error.code.as_str(), "launch_failed" | "workspace_untrusted") => {
            if let Some(problem) = problem_of(&error.code, &error.message, None) {
                failed(domain, task, problem, alondra_core::now_millis())?;
            }
        }
        _ => {}
    }
    Ok(())
}

