//! Closing the node of a task that is finished or cancelled: Alondra asks the node to clean up,
//! waits for it to say so (or for the wait to run out), closes its session with `claude rm` and
//! records what became of it, which is what makes a closing task done. A close that keeps
//! failing ends in a card the user answers; nothing is closed while the node still works or waits
//! for the user.
use crate::domain_github::execute_system;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{PostMergeCheck, TaskClosing};
use crate::node_close::{CloseOutcome, Sessions, close_node};
use alondra_core::domain::{
    AgentId, CardSpec, Command, DomainState, InteractionId, InteractionPayload, NodeEnd, PostMergeWork, QuestionOption,
    Scope, Task, TaskId, TaskState, clip,
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
pub(crate) const POST_MERGE_ASK: Duration = Duration::from_secs(3 * 60 * 60);
pub(crate) const POST_MERGE_ANSWER: Duration = Duration::from_secs(30 * 60);

pub(crate) struct Timing {
    /// How long a node may take to clean up before Alondra closes it anyway; `None` closes it only
    /// once it says it cleaned up.
    pub wait: Option<Duration>,
    pub retry: Duration,
    pub tick: Duration,
    /// How long a node may say it works after the merge before Alondra asks whether it still does.
    pub post_merge_ask: Duration,
    /// How long Alondra waits for that answer from an idle node before asking the user.
    pub post_merge_answer: Duration,
}

impl Timing {
    pub(crate) fn new(wait: Option<Duration>) -> Self {
        Self { wait, retry: RETRY, tick: TICK, post_merge_ask: POST_MERGE_ASK, post_merge_answer: POST_MERGE_ANSWER }
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
        forget_post_merge_check(domain, &row.task_id)?;
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

/// Records what became of the node. A task that went back to work while its node was removed is
/// released like one whose node the user closed: what a relaunch needs is kept, the orchestrator
/// is told, and the changes asked for reach the node that replaces it.
fn record_end(domain: &DomainRuntime, task_id: &TaskId, end: NodeEnd, agent: Option<&AgentId>) {
    let before = domain.state().ok();
    let back_to_work = matches!(end, NodeEnd::Closed | NodeEnd::Gone)
        && before.as_ref().and_then(|state| state.tasks.get(task_id)).is_some_and(|task| !task.state.terminal() && task.wrap_up.asked_at.is_none());
    if let Err(error) = execute_system(domain, Command::RecordNodeEnd { task_id: task_id.clone(), end, agent_id: agent.cloned() }) {
        log!("could not record what became of the node of {task_id}: {error:?}");
        return;
    }
    let (Some(state), true) = (before, back_to_work) else { return };
    let (Some(task), Some(agent)) = (state.tasks.get(task_id), agent) else { return };
    let name = state.agents.get(agent).map_or_else(|| agent.0.clone(), |known| known.name.clone());
    domain.live.remember_closed_task(&task_id.0, crate::client_routes::ClosedNode { name, cwd: domain.live.mods.session(&agent.0).and_then(|reported| reported.cwd) });
    let _ = domain.tell_orchestrator(&format!("node-closed-while-working-{task_id}-{}", crate::auth::random_hex(4)), format!(
        "The node of “{}” ({task_id}) was closed while the user asked for changes on it. The task waits for a new node: start it with launch (task {task_id}); its mission carries the changes asked for.",
        task.title));
}

fn finish(domain: &DomainRuntime, row: &TaskClosing, agent: Option<&AgentId>, end: NodeEnd) -> Result<()> {
    record_end(domain, &row.task_id, end, agent);
    forget_post_merge_check(domain, &row.task_id)?;
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
        return finish(domain, &row, None, NodeEnd::Gone);
    };
    let orchestrator = domain.live.node(&agent.0).is_some_and(|node| node.is_orchestrator);
    if orchestrator || !domain.launched_by_alondra(&agent)? {
        return finish(domain, &row, Some(&agent), NodeEnd::UsersOwn);
    }
    if let Some(card_id) = row.problem_id.clone() {
        match card(&state, &card_id) {
            Card::Leave => return finish(domain, &row, Some(&agent), NodeEnd::KeptByUser),
            Card::Retry | Card::Withdrawn => {
                row.failures = 0;
                row.problem_id = None;
                row.next_attempt = 0;
                domain.store.lock().expect("domain store poisoned").save_task_closing(&row)?;
            }
            Card::Unanswered => return Ok(()),
        }
    }
    let mut close_now = false;
    match (&kind, &task.wrap_up.post_merge_work) {
        (Due::Cleanup, Some(work)) => match watch_post_merge_work(domain, &state, timing, now, task, &agent, work)? {
            Watch::Hold => return hold_while_working_after_merge(domain, now, row),
            Watch::CloseNow => close_now = true,
        },
        _ => forget_post_merge_check(domain, task_id)?,
    }
    let cleaned_at = task.wrap_up.cleaned_at.map(|revision| revision as i64);
    let confirmed = cleaned_at.is_some() && cleaned_at > row.attempted_cleaned_at;
    let ready = match kind {
        Due::Cancelled => true,
        Due::Cleanup => confirmed
            || close_now
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
        let first = row.ready_since.is_none();
        let waiting_since = *row.ready_since.get_or_insert(now);
        if first {
            domain.store.lock().expect("domain store poisoned").save_task_closing(&row)?;
        }
        if now - waiting_since > millis(timing.wait.unwrap_or(BUSY_FALLBACK)) {
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
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            row.rm_started_at = None;
            domain.store.lock().expect("domain store poisoned").save_task_closing(&row)?;
            return Err(error);
        }
    };
    match outcome {
        CloseOutcome::Closed { .. } => finish(domain, &row, Some(&agent), NodeEnd::Closed),
        CloseOutcome::NotListed => {
            crate::node_revival::user_closed(domain, &agent.0)?;
            finish(domain, &row, Some(&agent), if interrupted { NodeEnd::Closed } else { NodeEnd::Gone })
        }
        CloseOutcome::Refused { message, .. } | CloseOutcome::Failed { message, .. } => {
            failure(domain, timing, now, task, &agent, row, &message, matches!(kind, Due::Cleanup))
        }
    }
}

enum Watch {
    Hold,
    CloseNow,
}

fn forget_post_merge_check(domain: &DomainRuntime, task_id: &TaskId) -> Result<()> {
    let check = domain.store.lock().expect("domain store poisoned").post_merge_check(task_id)?;
    let Some(check) = check else { return Ok(()) };
    if let Some(card) = &check.problem_id {
        let _ = domain.withdraw_problem(&InteractionId(card.clone()));
    }
    domain.store.lock().expect("domain store poisoned").forget_post_merge_check(task_id)
}

/// Work the node declared after the merge holds the close, but not forever: after
/// `post_merge_ask` Alondra asks the node whether it still runs; a node that is not working and
/// does not answer within `post_merge_answer` gets a card in front of the user, who closes it or
/// leaves it for another round. Declaring again, even with the same reason, is the answer.
fn watch_post_merge_work(
    domain: &DomainRuntime, state: &DomainState, timing: &Timing, now: i64, task: &Task, agent: &AgentId, work: &PostMergeWork,
) -> Result<Watch> {
    let declared_at = work.declared_at as i64;
    let stored = domain.store.lock().expect("domain store poisoned").post_merge_check(&task.id)?;
    let mut check = match stored {
        Some(check) if check.declared_at == declared_at => check,
        previous => {
            if let Some(card) = previous.and_then(|check| check.problem_id) {
                let _ = domain.withdraw_problem(&InteractionId(card));
            }
            PostMergeCheck { task_id: task.id.clone(), declared_at, since: now, asked_at: None, problem_id: None, close_now: false }
        }
    };
    if let Some(card_id) = check.problem_id.clone() {
        match card(state, &card_id) {
            Card::Unanswered => {}
            Card::Retry => check.close_now = true,
            Card::Leave | Card::Withdrawn => {
                check.since = now;
                check.asked_at = None;
                check.problem_id = None;
            }
        }
    } else if let Some(asked) = check.asked_at {
        let working = domain.live.node(&agent.0).is_some_and(|node| node.busy);
        if !working && now - asked >= millis(timing.post_merge_answer) {
            check.problem_id = raise_post_merge_card(domain, task, &work.reason, now - check.since);
        }
    } else if now - check.since >= millis(timing.post_merge_ask) {
        let text = still_working_text(&work.reason, &task.id, now - check.since, timing.post_merge_answer);
        let _ = domain.live.pushes.message(&agent.0, crate::node_push::ALONDRA, &text, false);
        check.asked_at = Some(now);
    }
    domain.store.lock().expect("domain store poisoned").save_post_merge_check(&check)?;
    Ok(if check.close_now { Watch::CloseNow } else { Watch::Hold })
}

fn elapsed(millis: i64) -> String {
    let minutes = millis.max(0) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

fn still_working_text(reason: &str, task: &TaskId, held: i64, answer: Duration) -> String {
    format!(
        "{held} ago you said this was still running after the merge: “{reason}”. If it still runs, call \
post_merge_work again with task_id {task} and the reason. If it ended, call it without reason, \
clean up and call cleanup_done. If you do not answer within {answer} while you are not working, \
Alondra asks the user whether to close this session.",
        held = elapsed(held),
        task = task.0,
        answer = elapsed(millis(answer)),
    )
}

fn raise_post_merge_card(domain: &DomainRuntime, task: &Task, reason: &str, held: i64) -> Option<String> {
    let id = format!("problem-post-merge-{}-{}", task.id, crate::auth::random_hex(6));
    let need = clip(&format!("Decide whether to close the session of “{}”", task.title), CardSpec::NEED_MAX);
    let why = clip(&format!(
        "Its PR is merged and its node has said for {} that it is still working: “{reason}”. It does not answer whether it still is.",
        elapsed(held)), CardSpec::WHY_MAX);
    let diagnosis = format!(
        "Task: “{title}”. The node said {held} ago that this was still running after the merge: “{reason}”. Alondra asked \
it whether it still runs, it did not answer, and it is not working now. Close the node: Alondra closes the session \
now, and anything still running in it stops. Leave it: the session stays open and Alondra asks the node again in \
a few hours.",
        title = task.title,
        held = elapsed(held),
    );
    let options = vec![
        QuestionOption { id: "close".into(), label: "Close the node".into(), description: None, inputs: vec![] },
        QuestionOption { id: "leave".into(), label: "Leave it".into(), description: None, inputs: vec![] },
    ];
    match domain.raise_problem(&id, Some(&format!("post-merge:{}", task.id)), Scope::Task { task_id: task.id.clone() }, need, why, diagnosis, options) {
        Ok(_) => Some(id),
        Err(error) => {
            log!("could not ask the user whether to close the session of {} after its work after the merge: {error:?}", task.id);
            None
        }
    }
}

/// The node said it still works on something after the merge: its margin starts over from now
/// on every pass, so it runs again in full once the node says it is done.
fn hold_while_working_after_merge(domain: &DomainRuntime, now: i64, mut row: TaskClosing) -> Result<()> {
    row.since = now;
    row.ready_since = None;
    row.failures = 0;
    row.last_error = None;
    row.next_attempt = 0;
    domain.store.lock().expect("domain store poisoned").save_task_closing(&row)
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
        let _ = domain.live.pushes.message(&agent.0, crate::node_push::ALONDRA, &text, false);
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
    match domain.raise_problem(&id, Some(&format!("close-session:{}", task.id)), scope, need, why, diagnosis, options) {
        Ok(_) => Some(id),
        Err(error) => {
            log!("could not tell the user Alondra cannot close the session of {}: {error:?}", task.id);
            None
        }
    }
}

#[cfg(test)]
mod tests;
