//! Starting a task as soon as it is ready: every task not started yet is launched by Alondra itself
//! with the launch plan it was created with, when the admission gate has room for it, by priority then
//! by how long it has had its plan; the mission of a task that waited on others carries what they
//! delivered. One automatic launch per task, ever: the claim is a durable row written before
//! `claude` runs, so a restart or a second pass never starts it twice. A start that fails is tried
//! again by itself and then put in front of the user (`start_failures`). A task whose node Alondra
//! closed for a reason of its own (a replacement that stopped short, a node closed while the user
//! asked for changes) is relaunched the same way, in the folder that node worked in.
use crate::engine::EngineOf as _;
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use alondra_engine_ports::gate::GateOf as _;
use alondra_core::clock;
use crate::locks::Poisonable as _;
use alondra_daemon_engine_model::LaunchBody;
use alondra_engine_ports::sdk::Relaunch;
use alondra_engine_kernel::start_failures;
use alondra_engine_ports::Runtime;
use crate::domain_store::{AutoLaunchReason, AutoLaunchState};
use alondra_domain_rules::{DomainError, DomainState, Task, TaskId, TaskState};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const TICK: Duration = Duration::from_secs(60);
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const LAST_BACKOFF: Duration = Duration::from_secs(5 * 60);

pub async fn run(domain: Arc<dyn Runtime>, home: PathBuf) {
    if let Err(error) = recover(&*domain) {
        log!("could not look at the automatic launches a restart interrupted: {error:#}");
    }
    let mut changed = domain.subscribe();
    let mut turn = domain.gate().subscribe();
    let mut backoff = FIRST_BACKOFF;
    loop {
        let pause = match pass(&*domain, &home).await {
            Ok(()) => {
                backoff = FIRST_BACKOFF;
                TICK
            }
            Err(error) => {
                log!("starting ready tasks failed, trying again in {} s: {error:#}", backoff.as_secs());
                let pause = backoff;
                backoff = (backoff * 2).min(LAST_BACKOFF);
                pause
            }
        };
        let woke = tokio::select! {
            change = changed.changed() => change.is_ok(),
            turn = turn.changed() => turn.is_ok(),
            () = clock::sleep(pause) => true,
        };
        if !woke {
            return;
        }
    }
}

/// A launch the daemon stopped in the middle of: kept when its node was assigned, else tried again.
/// A task an older daemon let exist without a launch plan is cancelled, saying so, and the
/// orchestrator is told to create it again with one if it is still needed.
pub(crate) fn recover(domain: &dyn Runtime) -> Result<()> {
    let state = domain.state();
    let rows = domain.store().lock_or_exit("the domain store").auto_launches()?;
    for row in rows.into_iter().filter(|row| row.state == AutoLaunchState::Launching) {
        let mut store = domain.store().lock_or_exit("the domain store");
        if state.tasks.get(&row.task_id).is_some_and(|task| task.agent_id.is_some()) {
            store.set_auto_launch(&row.task_id, AutoLaunchState::Launched, None, None, alondra_core::now_millis())?;
        } else {
            log!("retrying the automatic launch of {}, interrupted by a restart", row.task_id);
            store.forget_auto_launch(&row.task_id)?;
        }
    }
    domain.store().lock_or_exit("the domain store").forget_planless_blocks()?;
    cancel_planless(domain, &*domain.state())
}

const PLANLESS: &str = "It had no launch plan, so nothing would ever start it";

fn cancel_planless(domain: &dyn Runtime, state: &DomainState) -> Result<()> {
    let planned = domain.store().lock_or_exit("the domain store").planned_tasks()?;
    let planless: Vec<&Task> = state.tasks.values()
        .filter(|task| matches!(task.state, TaskState::NotStarted | TaskState::WaitingDependency) && !task.started && task.agent_id.is_none() && !task.archived)
        .filter(|task| !planned.contains_key(&task.id) && !state.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(&task.id)))
        .collect();
    for task in planless {
        let cancelled = alondra_engine_github::domain_github::execute_system(domain, alondra_domain_rules::Command::CancelTask { task_id: task.id.clone(), reason: None });
        if let Err(error) = cancelled {
            log!("could not cancel {}, which has no launch plan: {error:?}", task.id);
            continue;
        }
        domain.store().lock_or_exit("the domain store").set_cancel_reason(&task.id, PLANLESS)?;
        tell(domain, &format!("planless-cancelled-{}", task.id), format!(
            "Alondra cancelled “{}” ({}): it had no launch plan, so nothing would ever start it. If it is still needed, create it again with create_task and its launch plan.",
            task.title, task.id));
    }
    Ok(())
}

pub use alondra_engine_ports::launch::candidate;

pub async fn pass(domain: &dyn Runtime, home: &Path) -> Result<()> {
    crate::start_retries::react(domain, home).await?;
    let state = domain.state();
    let now = alondra_core::now_millis();
    let (planned, retries) = {
        let store = domain.store().lock_or_exit("the domain store");
        (store.planned_tasks()?, store.start_retries()?)
    };
    let mut ready: Vec<&Task> = state.tasks.values().filter(|task| candidate(&state, task)).collect();
    ready.sort_by_key(|task| (task.priority, planned.get(&task.id).copied().unwrap_or(now), &task.id));
    for task in ready {
        let relaunching = match domain.engine().closed_task(&task.id).map(|node| node.relaunch) {
            Some(Relaunch::AskUser) => continue,
            Some(Relaunch::ByItself) => true,
            None => false,
        };
        if domain.gate().held_by_user(&task.id) || (!relaunching && domain.store().lock_or_exit("the domain store").task_was_launched(&task.id)?) {
            continue;
        }
        let (row, plan) = {
            let store = domain.store().lock_or_exit("the domain store");
            (store.auto_launch(&task.id)?, store.launch_plan(&task.id)?)
        };
        if retries.get(&task.id).is_some_and(|(_, at)| *at > now) {
            continue;
        }
        if domain.engine().api_down(now) {
            continue;
        }
        let usage_out = domain.engine().usage_pause().is_some();
        let retry = match row.as_ref().map(|row| (row.state, row.reason)) {
            None => false,
            Some((AutoLaunchState::Launched, _)) if relaunching => {
                domain.store().lock_or_exit("the domain store").forget_auto_launch(&task.id)?;
                false
            }
            Some((AutoLaunchState::Launched | AutoLaunchState::Launching, _)) => continue,
            Some((AutoLaunchState::Blocked, Some(AutoLaunchReason::UsageExhausted))) if !usage_out => true,
            Some((AutoLaunchState::Blocked, _)) => continue,
        };
        let block = |reason: AutoLaunchReason, detail: Option<&str>| -> Result<()> {
            domain.store().lock_or_exit("the domain store").set_auto_launch(&task.id, AutoLaunchState::Blocked, Some(reason), detail, now)
        };
        if plan.is_none() && !relaunching { continue; }
        if usage_out {
            block(AutoLaunchReason::UsageExhausted, None)?;
            continue;
        }
        let waiting = alondra_engine_ports::admission::Waiting { key: alondra_engine_ports::admission::Key::Task(task.id.clone()), priority: task.priority, resuming: false,
            since: planned.get(&task.id).copied().unwrap_or(now) };
        if domain.gate().ask(waiting, now).is_err() {
            continue;
        }
        if !domain.store().lock_or_exit("the domain store").claim_auto_launch(&task.id, now, retry)? {
            continue;
        }
        let Some(_launching) = domain.engine().begin_launch(&task.id) else {
            domain.store().lock_or_exit("the domain store").forget_auto_launch(&task.id)?;
            continue;
        };
        let plan = domain.stored_launch_plan(&task.id)?;
        let launched = match &plan {
            _ if relaunching => domain.engine().relaunch(home, &task.id, true).await,
            Some(plan) => domain.engine().launch_planned(home, LaunchBody::from_plan(task.id.clone(), plan).to_queue()).await,
            None => {
                domain.store().lock_or_exit("the domain store").forget_auto_launch(&task.id)?;
                continue;
            }
        };
        settle(domain, &task.id, plan.as_ref().map(|plan| plan.cwd.as_str()), launched, now)?;
    }
    Ok(())
}

/// What an automatic launch leaves behind: a refusal that clears by itself waits for a later pass,
/// a task that ended while it started just leaves, and any other refusal is a failed start.
pub fn settle(domain: &dyn Runtime, task: &TaskId, cwd: Option<&str>, launched: Result<serde_json::Value, DomainError>, now: i64) -> Result<()> {
    let block = |reason: AutoLaunchReason| -> Result<()> {
        domain.store().lock_or_exit("the domain store").set_auto_launch(task, AutoLaunchState::Blocked, Some(reason), None, now)
    };
    match launched {
        Ok(_) => domain.store().lock_or_exit("the domain store").set_start_retry(task, None)?,
        Err(_) if start_failures::ended(domain, task)? => {}
        Err(error) => match error.code.as_str() {
            "usage_exhausted" => block(AutoLaunchReason::UsageExhausted)?,
            "account_unavailable" | "account_check_in_progress" => {
                domain.gate().not_started(&alondra_engine_ports::admission::Key::Task(task.clone()));
                domain.store().lock_or_exit("the domain store").forget_auto_launch(task)?;
            }
            "worktrees_unavailable" | "disk_low" | "plan_review_pending" => domain.store().lock_or_exit("the domain store").forget_auto_launch(task)?,
            "conflict" => {
                let taken = domain.state().tasks.get(task).is_some_and(|task| task.agent_id.is_some());
                let mut store = domain.store().lock_or_exit("the domain store");
                if taken {
                    store.set_auto_launch(task, AutoLaunchState::Launched, None, None, now)?;
                } else {
                    store.forget_auto_launch(task)?;
                }
            }
            code => {
                domain.gate().not_started(&alondra_engine_ports::admission::Key::Task(task.clone()));
                if let Some(problem) = start_failures::problem_of(&*domain.ports().accounts(), code, &error.message, cwd) {
                    start_failures::failed(domain, task, problem, now)?;
                }
                log!("could not start {task} by itself: {}", error.message);
            }
        },
    }
    Ok(())
}

fn tell(domain: &dyn Runtime, id: &str, body: String) {
    if let Err(error) = domain.tell_orchestrator(id, None, body) {
        log!("could not tell the orchestrator about {id}: {error:?}");
    }
}

#[cfg(all(test, feature = "whole-engine"))]
pub use crate::engine_fixtures::launches as fixtures;
#[cfg(all(test, feature = "whole-engine", not(feature = "real-hosts")))]
mod tests;
