//! Closing the node of a task that is finished or cancelled: Alondra asks the node to clean up,
//! waits for it to say so (or for the wait to run out), closes its session with `claude rm` and
//! records what became of it, which is what makes a closing task done. A close that keeps
//! failing ends in a card the user answers; nothing is closed while the node still works or waits
//! for the user.
use crate::domain_github::execute_system;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::TaskClosing;
use crate::node_close::{CloseOutcome, Sessions, close_node};
use alondra_core::domain::{
    AgentId, CardSpec, Command, DomainState, InteractionId, InteractionPayload, NodeEnd, QuestionOption, Scope, Task,
    TaskId, TaskState, clip,
};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

pub(crate) const RETRY: Duration = Duration::from_secs(5 * 60);
pub(crate) const TICK: Duration = Duration::from_secs(30);
const MAX_FAILURES: u32 = 3;
const BUSY_FALLBACK: Duration = Duration::from_secs(30 * 60);
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const LAST_BACKOFF: Duration = Duration::from_secs(5 * 60);

pub(crate) struct Timing {
    /// How long a node may take to clean up before Alondra closes it anyway; `None` closes it only
    /// once it says it cleaned up.
    pub wait: Option<Duration>,
    pub retry: Duration,
    pub tick: Duration,
}

impl Timing {
    pub(crate) fn new(wait: Option<Duration>) -> Self {
        Self { wait, retry: RETRY, tick: TICK }
    }
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, sessions: Arc<dyn Sessions>, timing: Timing) {
    let mut changed = domain.subscribe();
    let mut backoff = FIRST_BACKOFF;
    loop {
        let pause = match pass(&domain, sessions.as_ref(), &timing, alondra_core::now_millis()).await {
            Ok(()) => {
                backoff = FIRST_BACKOFF;
                timing.tick
            }
            Err(error) => {
                log!("closing the nodes of finished tasks failed, trying again in {} s: {error:#}", backoff.as_secs());
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

enum Due {
    Cleanup,
    Cancelled,
}

fn due(state: &DomainState, task: &Task) -> Option<Due> {
    if state.in_cleanup(task) && state.wraps_up(task) {
        Some(Due::Cleanup)
    } else if task.state == TaskState::Cancelled && task.wrap_up.close_cancelled && task.wrap_up.node.is_none() {
        Some(Due::Cancelled)
    } else {
        None
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

pub(crate) async fn pass(domain: &DomainRuntime, sessions: &dyn Sessions, timing: &Timing, now: i64) -> Result<()> {
    let state = domain.state()?;
    let due_tasks: Vec<(TaskId, Due)> = state.tasks.values().filter_map(|task| due(&state, task).map(|kind| (task.id.clone(), kind))).collect();
    let rows = domain.store.lock().expect("domain store poisoned").task_closings()?;
    for row in rows.iter().filter(|row| !due_tasks.iter().any(|(id, _)| id == &row.task_id)) {
        if let Some(card) = &row.problem_id {
            let _ = domain.withdraw_problem(&InteractionId(card.clone()));
        }
        domain.store.lock().expect("domain store poisoned").forget_task_closing(&row.task_id)?;
    }
    for (task_id, kind) in due_tasks {
        close_one(domain, sessions, timing, now, &task_id, kind).await?;
    }
    Ok(())
}

/// What the user answered on the card raised after the third failure.
enum Card {
    Unanswered,
    Retry,
    Leave,
    Withdrawn,
}

fn card(state: &DomainState, id: &str) -> Card {
    let Some(card) = state.interactions.get(&InteractionId(id.to_owned())) else { return Card::Withdrawn };
    if card.withdrawn {
        return Card::Withdrawn;
    }
    match &card.payload {
        InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == "leave" => Card::Leave,
        InteractionPayload::Question { answer: Some(_), .. } => Card::Retry,
        _ => Card::Unanswered,
    }
}

fn record_end(domain: &DomainRuntime, task_id: &TaskId, end: NodeEnd) {
    if let Err(error) = execute_system(domain, Command::RecordNodeEnd { task_id: task_id.clone(), end }) {
        log!("could not record what became of the node of {task_id}: {error:?}");
    }
}

fn finish(domain: &DomainRuntime, row: &TaskClosing, end: NodeEnd) -> Result<()> {
    record_end(domain, &row.task_id, end);
    if let Some(card) = &row.problem_id {
        let _ = domain.withdraw_problem(&InteractionId(card.clone()));
    }
    domain.store.lock().expect("domain store poisoned").forget_task_closing(&row.task_id)?;
    Ok(())
}

async fn close_one(domain: &DomainRuntime, sessions: &dyn Sessions, timing: &Timing, now: i64, task_id: &TaskId, kind: Due) -> Result<()> {
    let state = domain.state()?;
    let Some(task) = state.tasks.get(task_id) else { return Ok(()) };
    let mut row = domain.store.lock().expect("domain store poisoned").begin_task_closing(task_id, now)?;
    let Some(agent) = task.agent_id.clone() else {
        return finish(domain, &row, NodeEnd::Gone);
    };
    let orchestrator = domain.live.node(&agent.0).is_some_and(|node| node.is_orchestrator);
    if orchestrator || !domain.launched_by_alondra(&agent)? {
        return finish(domain, &row, NodeEnd::UsersOwn);
    }
    if let Some(card_id) = row.problem_id.clone() {
        match card(&state, &card_id) {
            Card::Leave => return finish(domain, &row, NodeEnd::KeptByUser),
            Card::Retry | Card::Withdrawn => {
                row.failures = 0;
                row.problem_id = None;
                row.next_attempt = 0;
                domain.store.lock().expect("domain store poisoned").save_task_closing(&row)?;
            }
            Card::Unanswered => return Ok(()),
        }
    }
    let cleaned_at = task.wrap_up.cleaned_at.map(|revision| revision as i64);
    let confirmed = cleaned_at.is_some() && cleaned_at > row.attempted_cleaned_at;
    let ready = match kind {
        Due::Cancelled => true,
        Due::Cleanup => confirmed
            || row.last_error.is_some()
            || timing.wait.is_some_and(|wait| now >= row.since + millis(wait)),
    };
    if !ready || (now < row.next_attempt && !confirmed) {
        return Ok(());
    }
    let node = domain.live.node(&agent.0);
    if node.as_ref().is_some_and(|node| node.waiting_permission) || crate::domain_holds::waits_on_user(&state, &agent.0) {
        return Ok(());
    }
    if node.as_ref().is_some_and(|node| node.busy) {
        if now - row.since > millis(timing.wait.unwrap_or(BUSY_FALLBACK)) {
            return failure(domain, timing, now, task, &agent, row, "it is still working", matches!(kind, Due::Cleanup));
        }
        return Ok(());
    }
    if !domain.live.begin_watch(&agent.0) {
        return Ok(());
    }
    let interrupted = row.rm_started_at.is_some();
    row.rm_started_at = Some(now);
    let saved = domain.store.lock().expect("domain store poisoned").save_task_closing(&row);
    let outcome = match saved {
        Ok(()) => close_node(domain, sessions, &agent, false).await,
        Err(error) => Err(error),
    };
    domain.live.end_watch(&agent.0);
    match outcome? {
        CloseOutcome::Closed { .. } => finish(domain, &row, NodeEnd::Closed),
        CloseOutcome::NotListed => {
            crate::node_revival::user_closed(domain, &agent.0)?;
            finish(domain, &row, if interrupted { NodeEnd::Closed } else { NodeEnd::Gone })
        }
        CloseOutcome::Refused { message, .. } | CloseOutcome::Failed { message, .. } => {
            failure(domain, timing, now, task, &agent, row, &message, matches!(kind, Due::Cleanup))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn failure(domain: &DomainRuntime, timing: &Timing, now: i64, task: &Task, agent: &AgentId, mut row: TaskClosing, reason: &str, cleanup: bool) -> Result<()> {
    row.failures += 1;
    row.last_error = Some(reason.to_owned());
    row.next_attempt = now + millis(timing.retry);
    row.attempted_cleaned_at = task.wrap_up.cleaned_at.map(|revision| revision as i64);
    row.rm_started_at = None;
    if cleanup {
        let text = close_failed_text(reason, &task.id, timing.retry.as_secs() / 60);
        let _ = domain.live.pushes.message(&agent.0, &text, false);
    }
    if row.failures >= MAX_FAILURES && row.problem_id.is_none() {
        row.problem_id = raise_card(domain, task, reason, cleanup);
    }
    domain.store.lock().expect("domain store poisoned").save_task_closing(&row)?;
    Ok(())
}

fn close_failed_text(reason: &str, task: &TaskId, minutes: u64) -> String {
    format!(
        "Alondra could not close this session yet: {reason}. If this comes from your \
worktree, fix it now: delete what you generated that git does not track with the project's own \
tools, and if some of it is work that still matters, push it to a branch of its own and tell the \
orchestrator about it (ask with recipient orchestrator). Then call cleanup_done with task_id \
{task}. Alondra tries again in {minutes} minutes either way.",
        reason = reason.trim().trim_end_matches('.'),
        task = task.0,
    )
}

fn raise_card(domain: &DomainRuntime, task: &Task, reason: &str, cleanup: bool) -> Option<String> {
    let id = format!("problem-close-{}-{}", task.id, crate::auth::random_hex(6));
    let scope = if cleanup {
        Scope::Task { task_id: task.id.clone() }
    } else {
        Scope::Orchestrator { request_id: Some(task.request_id.clone()) }
    };
    let need = clip(&format!("Alondra can't close the session of “{}”", task.title), CardSpec::NEED_MAX);
    let why = if cleanup {
        clip("It tried 3 times and failed each time, so the task stays open until the session is closed or you leave it open.", CardSpec::WHY_MAX)
    } else {
        clip("It tried 3 times and failed each time.", CardSpec::WHY_MAX)
    };
    let diagnosis = format!(
        "Last error: {}. Try again once that is fixed, or leave the session open: the task then finishes and you can close the session yourself later from Fleet.",
        reason.trim()
    );
    let options = vec![
        QuestionOption { id: "retry".into(), label: "Try again".into(), description: None, inputs: vec![] },
        QuestionOption { id: "leave".into(), label: "Leave it open".into(), description: None, inputs: vec![] },
    ];
    match domain.raise_problem(&id, scope, need, why, diagnosis, options) {
        Ok(_) => Some(id),
        Err(error) => {
            log!("could not tell the user Alondra cannot close the session of {}: {error:?}", task.id);
            None
        }
    }
}

#[cfg(test)]
mod tests;
