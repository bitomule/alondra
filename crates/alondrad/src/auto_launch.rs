//! Starting a task as soon as it is ready: a task that waited on others is launched by Alondra
//! itself with the launch plan its orchestrator gave it, and its mission carries what the earlier
//! tasks delivered. One automatic launch per task, ever: the claim is a durable row written before
//! `claude` runs, so a restart or a second pass never starts it twice.
use crate::domain_orchestrator_tools::{LaunchBody, Origin, launch_from, request_choices};
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{AutoLaunchReason, AutoLaunchState};
use crate::orchestrate::Launcher;
use alondra_core::domain::{Actor, DomainState, Task, TaskState};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const TICK: Duration = Duration::from_secs(60);
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const LAST_BACKOFF: Duration = Duration::from_secs(5 * 60);

pub(crate) async fn run(domain: Arc<DomainRuntime>, home: PathBuf, launcher: impl Launcher) {
    if let Err(error) = recover(&domain) {
        log!("could not look at the automatic launches a restart interrupted: {error:#}");
    }
    let mut changed = domain.subscribe();
    let mut backoff = FIRST_BACKOFF;
    loop {
        let pause = match pass(&domain, &home, &launcher).await {
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
            () = tokio::time::sleep(pause) => true,
        };
        if !woke {
            return;
        }
    }
}

/// A launch the daemon stopped in the middle of: kept when its node was assigned, else tried again.
pub(crate) fn recover(domain: &DomainRuntime) -> Result<()> {
    let state = domain.state()?;
    let rows = domain.store.lock().expect("domain store poisoned").auto_launches()?;
    for row in rows.into_iter().filter(|row| row.state == AutoLaunchState::Launching) {
        let mut store = domain.store.lock().expect("domain store poisoned");
        if state.tasks.get(&row.task_id).is_some_and(|task| task.agent_id.is_some()) {
            store.set_auto_launch(&row.task_id, AutoLaunchState::Launched, None, None, alondra_core::now_millis())?;
        } else {
            log!("retrying the automatic launch of {}, interrupted by a restart", row.task_id);
            store.forget_auto_launch(&row.task_id)?;
        }
    }
    Ok(())
}

fn candidate(state: &DomainState, task: &Task) -> bool {
    task.state == TaskState::NotStarted
        && !task.started
        && task.agent_id.is_none()
        && !task.archived
        && (!task.dependencies.is_empty() || !task.delivered_dependencies.is_empty() || task.stack.is_some())
        && state.requests.get(&task.request_id).is_some_and(|request| !request.closed)
        && !state.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(&task.id))
}

pub(crate) async fn pass(domain: &DomainRuntime, home: &Path, launcher: &impl Launcher) -> Result<()> {
    let state = domain.state()?;
    tell_about_cancelled_dependencies(domain, &state);
    let now = alondra_core::now_millis();
    for task in state.tasks.values().filter(|task| candidate(&state, task)) {
        if domain.live.closed_task(&task.id.0).is_some() || domain.store.lock().expect("domain store poisoned").task_was_launched(&task.id)? {
            continue;
        }
        let (row, plan) = {
            let store = domain.store.lock().expect("domain store poisoned");
            (store.auto_launch(&task.id)?, store.launch_plan(&task.id)?)
        };
        let usage_out = domain.live.usage_pause.current().is_some();
        let disk_low = domain.live.machine_watch.disk_low();
        let retry = match row.as_ref().map(|row| (row.state, row.reason)) {
            None => false,
            Some((AutoLaunchState::Launched | AutoLaunchState::Launching, _)) => continue,
            Some((AutoLaunchState::Blocked, Some(AutoLaunchReason::UsageExhausted))) if !usage_out => true,
            Some((AutoLaunchState::Blocked, Some(AutoLaunchReason::NoPlan))) if plan.is_some() => true,
            Some((AutoLaunchState::Blocked, _)) => continue,
        };
        let block = |reason: AutoLaunchReason, detail: Option<&str>| -> Result<()> {
            domain.store.lock().expect("domain store poisoned").set_auto_launch(&task.id, AutoLaunchState::Blocked, Some(reason), detail, now)
        };
        let Some(plan) = plan else {
            if row.is_none() {
                block(AutoLaunchReason::NoPlan, None)?;
                tell(domain, &format!("auto-launch-no-plan-{}", task.id), format!(
                    "“{}” ({}) can start now: everything it depends on is done. Alondra did not start it because it has no launch plan. Start it with launch (task {}), or give it a plan with set_launch_plan and Alondra starts it.",
                    task.title, task.id, task.id));
            }
            continue;
        };
        if usage_out {
            block(AutoLaunchReason::UsageExhausted, None)?;
            continue;
        }
        // The user has the disk card; the task stays unclaimed and starts on a pass after it clears.
        if disk_low {
            continue;
        }
        if !domain.store.lock().expect("domain store poisoned").claim_auto_launch(&task.id, now, retry)? {
            continue;
        }
        if !domain.live.begin_launch(&task.id.0) {
            domain.store.lock().expect("domain store poisoned").forget_auto_launch(&task.id)?;
            continue;
        }
        let choices = |request: &alondra_core::domain::RequestId| request_choices(domain, request).ok().flatten();
        let launched = launch_from(domain, home, &Actor::System, LaunchBody::from_plan(task.id.clone(), &plan), choices, launcher, Origin::Auto).await;
        domain.live.end_launch(&task.id.0);
        match launched {
            Ok(_) => {}
            Err(error) => match error.code.as_str() {
                "usage_exhausted" => block(AutoLaunchReason::UsageExhausted, None)?,
                "disk_low" => domain.store.lock().expect("domain store poisoned").forget_auto_launch(&task.id)?,
                "workspace_untrusted" => {
                    block(AutoLaunchReason::WorkspaceUntrusted, Some(&error.message))?;
                    tell(domain, &format!("auto-launch-untrusted-{}", task.id), format!(
                        "Alondra could not start “{}” ({}) by itself: {} Ask the user to trust the repository (ask, with those steps), then start it with launch (task {}).",
                        task.title, task.id, sentence(&error.message), task.id));
                }
                "conflict" => {
                    let taken = domain.state()?.tasks.get(&task.id).is_some_and(|task| task.agent_id.is_some());
                    let mut store = domain.store.lock().expect("domain store poisoned");
                    if taken {
                        store.set_auto_launch(&task.id, AutoLaunchState::Launched, None, None, now)?;
                    } else {
                        store.forget_auto_launch(&task.id)?;
                    }
                }
                _ => {
                    block(AutoLaunchReason::LaunchFailed, Some(&error.message))?;
                    tell(domain, &format!("auto-launch-failed-{}", task.id), format!(
                        "Alondra could not start “{}” ({}) by itself: {} Start it with launch (task {}) once the cause is fixed, or raise_problem if only the user can fix it.",
                        task.title, task.id, sentence(&error.message), task.id));
                }
            },
        }
    }
    Ok(())
}

fn sentence(message: &str) -> String {
    let message = message.trim();
    if message.ends_with(['.', '!', '?']) { message.to_owned() } else { format!("{message}.") }
}

fn tell(domain: &DomainRuntime, id: &str, body: String) {
    if let Err(error) = domain.tell_orchestrator(id, body) {
        log!("could not tell the orchestrator about {id}: {error:?}");
    }
}

/// A task whose dependency was cancelled waits forever: the orchestrator decides what to do.
fn tell_about_cancelled_dependencies(domain: &DomainRuntime, state: &DomainState) {
    for task in state.tasks.values().filter(|task| task.state == TaskState::WaitingDependency) {
        for dependency in task.dependencies.iter().chain(&task.delivered_dependencies).filter_map(|id| state.tasks.get(id)).filter(|dep| dep.state == TaskState::Cancelled) {
            let id = format!("dependency-cancelled-{}-{}", task.id, dependency.id);
            if state.interactions.contains_key(&alondra_core::domain::InteractionId(id.clone())) {
                continue;
            }
            tell(domain, &id, format!(
                "“{}” was cancelled, so “{}” ({}), which depends on it, cannot start. Change its dependencies with set_dependencies, or cancel it with cancel_task.",
                dependency.title, task.title, task.id));
        }
    }
}

#[cfg(test)]
mod tests;
