//! Closing the node of a task that is finished or cancelled: Alondra asks the node to clean up,
//! waits for it to say so (or, while it is idle, for the configured wait to run out), closes its
//! session with `claude rm` and records what became of it, which is what makes a closing task done.
//! A node of a finished task is closed only between turns, however long its turn runs; only a
//! cancelled task's node has its turn cut. Work it declared after the merge holds the close until
//! a fact ends it, never a clock. A close that keeps failing ends in a card the user answers.
//! Nothing is closed while the node waits for the user.
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use crate::blocking_work::blocking;
use crate::sdk_owner::SdkOwners as _;
use crate::engine::EngineOf as _;
use alondra_core::clock;
use alondra_domain_model::SystemCard;
use crate::locks::Poisonable as _;
use alondra_engine_github::domain_github::execute_system;
use alondra_engine_ports::Runtime;
use crate::domain_store::TaskClosing;
use alondra_engine_ports::node_close::{CloseOutcome, Sessions};
use alondra_engine_ports::worktrees::Unreachable;
use alondra_engine_ports::worktrees::TaskWorktree;
use alondra_domain_model::close_problem::{CLOSE_PROBLEM_PREFIX, CloseProblem, CloseStop};
use alondra_view_model::Lang;
use alondra_domain_model::{
    Actor, AgentId, Command, CommandEnvelope, PROTOCOL_VERSION, DomainState, InteractionId, InteractionPayload, NodeEnd, Scope, Task, TaskId,
};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub(crate) const RETRY: Duration = Duration::from_secs(5 * 60);
pub(crate) const TICK: Duration = Duration::from_secs(30);
const MAX_FAILURES: u32 = 3;
const STILL_WORKING: &str = "it is still working";
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const LAST_BACKOFF: Duration = Duration::from_secs(5 * 60);

pub struct Timing {
    /// How long a node of `claude --bg` may take to clean up before Alondra closes it anyway;
    /// `None` closes it only once it says it cleaned up. An SDK node closes on the rest of a turn
    /// after the request instead, which its lifecycle reports.
    pub wait: Option<Duration>,
    pub retry: Duration,
    pub tick: Duration,
}

impl Timing {
    pub fn new(wait: Option<Duration>) -> Self {
        Self { wait, retry: RETRY, tick: TICK }
    }

    /// A sandbox with fast timing looks again every couple of seconds.
    pub fn sandbox(wait: Option<Duration>) -> Self {
        Self { wait, retry: Duration::from_secs(10), tick: Duration::from_secs(2) }
    }
}

pub async fn run(domain: Arc<dyn Runtime>, sessions: Arc<dyn Sessions>, timing: Timing) {
    let mut changed = domain.subscribe();
    let mut backoff = FIRST_BACKOFF;
    match domain.keep_attachments() {
        Ok((kept, lost)) => {
            if kept > 0 || !lost.is_empty() {
                log!("gave {kept} attachments their own copy; {} could not be kept: {}", lost.len(), lost.join("; "));
            }
        }
        Err(error) => log!("could not keep the attachments of old cards: {error:#}"),
    }
    if let Some((backend, _)) = domain.sdk() {
        match crate::ended_hosts::sweep(&*domain, &backend.state_root) {
            Ok(0) => {}
            Ok(removed) => log!("deleted the folders of {removed} ended hosts whose task is over or has a later host"),
            Err(error) => log!("could not delete the folders of ended hosts: {error}"),
        }
    }
    if let Err(error) = domain.engine().sweep_cancelled_worktrees().await {
        log!("could not settle the worktrees of cancelled tasks: {error:#}");
    }
    if let Err(error) = domain.engine().sweep_worktree_shells().await {
        log!("could not delete what ended tasks left of their worktrees: {error:#}");
    }
    match blocking(|| crate::host_folders::sweep(&*domain, &|agent| domain.owns_sdk_host(agent))) {
        0 => {}
        removed => log!("deleted the folders of {removed} hosts that nothing needs any more"),
    }
    loop {
        let pause = match clock::real_time(pass(&*domain, sessions.as_ref(), &timing, alondra_core::now_millis())).await {
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
            () = clock::sleep(pause) => true,
        };
        if !woke {
            return;
        }
    }
}

enum Due {
    Cleanup,
    Cancelled,
    /// The task ends with no node to close: what it owes is its worktree, if it still has one,
    /// and its place in the launch queue. Alondra removes them and records the receipt.
    Unattended,
}

fn due(state: &DomainState, task: &Task) -> Option<Due> {
    if state.in_cleanup(task) && state.wraps_up(task) {
        Some(Due::Cleanup)
    } else if task.cancelled() && task.wrap_up.close_cancelled && task.wrap_up.node.is_none() {
        Some(Due::Cancelled)
    } else if task.ending() && !state.wraps_up(task) {
        Some(Due::Unattended)
    } else {
        None
    }
}

/// A task cancelled before any node took it owes only its place in the launch queue: that is
/// removed and its receipt recorded right after the commit that cancelled it, so the user never
/// waits for the closer's pass to read it as cancelled. One with a worktree left behind waits for
/// the closer, which releases worktrees.
pub(crate) fn settle_unattended(domain: &dyn Runtime, state: &DomainState) {
    for task in state.tasks.values().filter(|task| matches!(due(state, task), Some(Due::Unattended))) {
        if task.open_worktrees().next().is_some() {
            continue;
        }
        match domain.task_worktree(&task.id) {
            Ok(None) => {}
            Ok(Some(_)) => continue,
            Err(error) => {
                log!("could not look up the worktree of {}: {error:#}", task.id);
                continue;
            }
        }
        let dequeued = {
            let mut store = domain.store().lock_or_exit("the domain store");
            store.dequeue_launch(&task.id).and_then(|()| store.forget_auto_launch(&task.id))
        };
        if let Err(error) = dequeued {
            log!("could not take {} out of the launch queue: {error:#}", task.id);
            continue;
        }
        if let Err(error) = execute_system(domain, Command::RecordTaskCleanup { task_id: task.id.clone() }) {
            log!("could not record the cleanup of {}: {error:?}", task.id);
        }
    }
}

/// Nothing of a task that ends without a node stays behind: not its worktree, not its launch.
/// The receipt makes the task done or cancelled.
async fn finish_unattended(domain: &dyn Runtime, task: &Task) -> Result<()> {
    if task.cancelled() {
        domain.engine().release_cancelled_worktrees(task).await?;
        if !task.worktrees.is_empty() && !domain.state().tasks.get(&task.id).is_some_and(Task::ending) {
            return Ok(());
        }
        return record_unattended(domain, task);
    }
    let left = domain.engine().release_worktrees(task).await?;
    if !left.is_empty() {
        let list: Vec<String> = left.iter().map(|(path, what)| format!("{path}: {what}")).collect();
        anyhow::bail!("the worktrees of {} hold work that is not saved: {}", task.id, list.join("; "));
    }
    if !task.worktrees.is_empty() && !domain.state().tasks.get(&task.id).is_some_and(Task::ending) {
        return Ok(());
    }
    let dir = domain.task_worktree(&task.id)?.filter(|dir| !task.worktrees.contains_key(dir));
    if let Some(dir) = &dir { domain.engine().worktree_ready(dir).await?; }
    if let Some(dir) = dir.filter(|dir| domain.engine().may_handle_worktree(dir).is_ok() && domain.engine().worktree_heir(task, dir).is_ok_and(|heir| heir.is_none())) {
        match domain.ports().worktrees().release_finished_worktree(Path::new(&dir), Some(&alondra_engine_ports::worktrees::slug(&task.id.0)), false, domain.state().merged_pr(task).is_some()).await {
            Ok(TaskWorktree::Unsaved(left)) => anyhow::bail!("the worktree of {} has work that is not saved: {left}", task.id),
            Ok(TaskWorktree::Removed { freed }) => log!("deleting the worktree {dir} freed {}", alondra_core::machine_incident::bytes_text(freed, alondra_view_model::Lang::En)),
            Ok(TaskWorktree::Unavailable(why)) => return Err(Unreachable(why).into()),
            Ok(_) => {}
            Err(why) => anyhow::bail!("the worktree of {} could not be released: {why}", task.id),
        }
    }
    record_unattended(domain, task)
}

fn record_unattended(domain: &dyn Runtime, task: &Task) -> Result<()> {
    {
        let mut store = domain.store().lock_or_exit("the domain store");
        store.dequeue_launch(&task.id)?;
        store.forget_auto_launch(&task.id)?;
    }
    execute_system(domain, Command::RecordTaskCleanup { task_id: task.id.clone() }).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(())
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

pub async fn pass(domain: &dyn Runtime, sessions: &dyn Sessions, timing: &Timing, now: i64) -> Result<()> {
    if let Err(error) = domain.engine().apply_cancelled_worktree_answers().await {
        log!("could not delete the worktrees the user chose to delete: {error:#}");
    }
    let state = domain.state();
    let due_tasks: Vec<(TaskId, Due)> = state.tasks.values().filter_map(|task| due(&state, task).map(|kind| (task.id.clone(), kind))).collect();
    let rows = domain.store().lock_or_exit("the domain store").task_closings()?;
    for row in rows.iter().filter(|row| !due_tasks.iter().any(|(id, _)| id == &row.task_id)) {
        if let Some(card) = &row.problem_id {
            let _ = domain.withdraw_problem(&InteractionId(card.clone()));
        }
        forget_post_merge_check(domain, &row.task_id)?;
        domain.store().lock_or_exit("the domain store").forget_task_closing(&row.task_id)?;
    }
    let mut failed = Vec::new();
    for (task_id, kind) in due_tasks {
        if let Err(error) = close_one(domain, sessions, timing, now, &task_id, kind).await {
            let reason = format!("{error:#}");
            let state = domain.state();
            if let Some(task) = state.tasks.get(&task_id) {
                let row = domain.store().lock_or_exit("the domain store").begin_task_closing(&task_id, now)?;
                if error.downcast_ref::<Unreachable>().is_some() {
                    wait_for_folder(domain, timing, now, task, row, &reason)?;
                    continue;
                }
                let row = retry_later(timing, now, task, row, &reason);
                count_failure(domain, task, row, CloseStop::Error, &reason, state.in_cleanup(task))?;
            }
            failed.push(format!("{task_id}: {reason}"));
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(anyhow::anyhow!(failed.join("; "))) }
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
/// released like one whose node the user closed, but Alondra starts its next node by itself: the
/// domain keeps what a relaunch needs with the release, and the changes asked for reach the node
/// that replaces it.
fn record_end(domain: &dyn Runtime, task_id: &TaskId, end: NodeEnd, agent: Option<&AgentId>) -> Result<()> {
    if let Err(error) = execute_system(domain, Command::RecordNodeEnd { task_id: task_id.clone(), end, agent_id: agent.cloned() }) {
        if let Some(agent) = agent {
            let current = domain.state();
            // The old node's receipt no longer belongs to a task another node took over.
            if current.tasks.get(task_id).is_some_and(|task| task.agent_id.as_ref() != Some(agent)) {
                return Ok(());
            }
        }
        return Err(anyhow::anyhow!("recording the node end: {error:?}"));
    }
    if domain.state().tasks.get(task_id).is_some_and(|task| task.released.is_some()) {
        log!("the node of {task_id} was closed while the user asked for changes on it; Alondra starts a new one");
    }
    Ok(())
}

fn finish(domain: &dyn Runtime, row: &TaskClosing, agent: Option<&AgentId>, end: NodeEnd) -> Result<()> {
    if let Some(agent) = agent {
        domain.engine().set_close_interrupted(&agent.0, None);
    }
    record_end(domain, &row.task_id, end, agent)?;
    forget_post_merge_check(domain, &row.task_id)?;
    if domain.state().tasks.get(&row.task_id).is_some_and(Task::over) {
        if let Err(error) = crate::files_retention::remove_task_scratch(domain.home(), &row.task_id.0) {
            log!("could not remove the scratch files of {}: {error}", row.task_id);
        }
        crate::host_folders::task_closed(domain, &row.task_id, &|agent| domain.owns_sdk_host(agent));
    }
    if let Some(card) = &row.problem_id {
        let _ = domain.withdraw_problem(&InteractionId(card.clone()));
    }
    domain.store().lock_or_exit("the domain store").forget_task_closing(&row.task_id)?;
    Ok(())
}

async fn close_one(domain: &dyn Runtime, sessions: &dyn Sessions, timing: &Timing, now: i64, task_id: &TaskId, kind: Due) -> Result<()> {
    let state = domain.state();
    if state.tasks.get(task_id).is_some_and(|task| !state.closing_recovery_cards(task).is_empty()) {
        execute_system(domain, Command::ReconcileRecoveryCards { task_id: task_id.clone() })?;
    }
    let state = domain.state();
    let Some(task) = state.tasks.get(task_id) else { return Ok(()) };
    let mut row = domain.store().lock_or_exit("the domain store").begin_task_closing(task_id, now)?;
    if matches!(kind, Due::Cleanup) && !task.cancelled()
        && task.ending != Some(alondra_domain_model::TaskEnding::Cancelling)
        && !state.checklist_owed(task).is_empty()
    {
        if state.node_session_closed(task) {
            return finish(domain, &row, task.agent_id.as_ref(), NodeEnd::Gone);
        }
        return hold_while_working_after_merge(domain, now, row);
    }
    if let Some(card_id) = row.problem_id.clone() {
        match card(&state, &card_id) {
            // The user's answer stays the answer. It is recorded at once; a receipt that failed after
            // the card counted its failures is tried again on the wait.
            Card::Leave if row.failures > MAX_FAILURES && now < row.next_attempt => return Ok(()),
            Card::Leave => return finish(domain, &row, task.agent_id.as_ref(), NodeEnd::KeptByUser),
            Card::Retry | Card::Withdrawn => {
                if let Some(agent) = &task.agent_id {
                    domain.engine().set_close_interrupted(&agent.0, None);
                }
                row.failures = 0;
                row.problem_id = None;
                row.next_attempt = 0;
                domain.store().lock_or_exit("the domain store").save_task_closing(&row)?;
            }
            Card::Unanswered => return Ok(()),
        }
    }
    let confirmed = task.wrap_up.cleaned_at.is_some_and(|at| Some(at as i64) > row.attempted_cleaned_at);
    if now < row.next_attempt && !confirmed {
        return Ok(());
    }
    if matches!(kind, Due::Unattended) {
        finish_unattended(domain, task).await?;
        domain.store().lock_or_exit("the domain store").forget_task_closing(task_id)?;
        return Ok(());
    }
    let Some(agent) = task.agent_id.clone() else {
        return finish(domain, &row, None, NodeEnd::Gone);
    };
    let owned = domain.owns_sdk_host(&agent);
    if owned || crate::sdk_owner::runs_through_sdk(&state, &agent) {
        return close_sdk_node(domain, timing, now, &state, task, &agent, kind, row, owned).await;
    }
    let orchestrator = domain.engine().node(&agent.0).is_some_and(|node| node.is_orchestrator);
    if orchestrator || !domain.engine().launched_by_alondra(&agent)? {
        return finish(domain, &row, Some(&agent), NodeEnd::UsersOwn);
    }
    let session_over = state.node_session_closed(task);
    let mut close_now = session_over;
    forget_post_merge_check(domain, task_id)?;
    if matches!(kind, Due::Cleanup) && state.post_merge_owed(task) && !session_over {
        return hold_while_working_after_merge(domain, now, row);
    }
    close_now |= state.post_merge_closed_by_user(task_id);
    let cleaned_at = task.wrap_up.cleaned_at.map(|revision| revision as i64);
    let confirmed = cleaned_at.is_some() && cleaned_at > row.attempted_cleaned_at;
    let ready = match kind {
        Due::Cancelled | Due::Unattended => true,
        Due::Cleanup => confirmed
            || close_now
            || row.last_error.is_some()
            || timing.wait.is_some_and(|wait| now >= row.since + millis(wait)),
    };
    if !ready || (now < row.next_attempt && !confirmed) {
        return Ok(());
    }
    let left = if task.cancelled() { vec![] } else { domain.engine().worktree_leftovers(task).await };
    if !left.is_empty() {
        return hold_for_worktrees(domain, timing, now, task, &agent, row, &left, matches!(kind, Due::Cleanup));
    }
    let node = domain.engine().node(&agent.0);
    if node.as_ref().is_some_and(|node| node.waiting_permission) || alondra_engine_ports::admission::waits_on_user(&state, &agent.0) {
        return Ok(());
    }
    if node.as_ref().is_some_and(|node| node.busy) {
        if matches!(kind, Due::Cleanup) {
            return Ok(());
        }
        return match domain.engine().close_interrupted_at(&agent.0) {
            None => {
                domain.engine().interrupt(&agent.0);
                domain.engine().set_close_interrupted(&agent.0, Some(now));
                Ok(())
            }
            Some(at) if now - at < millis(timing.retry) => Ok(()),
            Some(_) => {
                domain.engine().set_close_interrupted(&agent.0, None);
                failure(domain, timing, now, task, &agent, row, STILL_WORKING, (CloseStop::Working, ""), matches!(kind, Due::Cleanup))
            }
        };
    }
    if !domain.engine().begin_watch(&agent.0) {
        return Ok(());
    }
    match domain.store().lock_or_exit("the domain store").claim_task_removal(task_id, now) {
        Ok(true) => {}
        claimed => {
            domain.engine().end_watch(&agent.0);
            claimed?;
            return Ok(());
        }
    }
    let interrupted = row.rm_started_at.is_some();
    row.rm_started_at = Some(now);
    let saved = domain.store().lock_or_exit("the domain store").save_task_closing(&row);
    let outcome = match saved {
        Ok(()) => domain.engine().close_node(sessions, &agent, false).await,
        Err(error) => Err(error),
    };
    domain.engine().end_watch(&agent.0);
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            row.rm_started_at = None;
            domain.store().lock_or_exit("the domain store").save_task_closing(&row)?;
            return Err(error);
        }
    };
    let cleanup = matches!(kind, Due::Cleanup);
    match outcome {
        CloseOutcome::Closed { .. } => finish_with_worktree(domain, timing, now, task, &agent, row, cleanup, NodeEnd::Closed).await,
        CloseOutcome::NotListed => {
            domain.engine().user_closed(&agent.0)?;
            let end = if interrupted { NodeEnd::Closed } else { NodeEnd::Gone };
            finish_with_worktree(domain, timing, now, task, &agent, row, cleanup, end).await
        }
        CloseOutcome::Refused { reasons, message } => {
            let (stop, detail) = match reasons.iter().find(|reason| reason.kind != "permission") {
                Some(reason) if reason.kind == "busy" => (CloseStop::Working, String::new()),
                Some(reason) if reason.kind == "worktree" => (CloseStop::Unsaved, reason.detail.clone().unwrap_or_default()),
                _ => (CloseStop::Error, message.clone()),
            };
            failure(domain, timing, now, task, &agent, row, &message, (stop, &detail), matches!(kind, Due::Cleanup))
        }
        CloseOutcome::Failed { message, .. } => {
            failure(domain, timing, now, task, &agent, row, &message, (CloseStop::Error, &message), matches!(kind, Due::Cleanup))
        }
    }
}

/// A node that runs through the Agent SDK is a process of Alondra's own: it is closed only by
/// stopping its host through its owner, never with `claude rm`, a push or the process table. A
/// cancelled task's node stops at once, a turn it is running interrupted first. A finished one stops once it owes no work after the merge
/// and, unless it confirmed its cleanup or the user chose to close it, once its lifecycle shows
/// nothing running: background work keeps it open, with no clock that cuts it. Its end is recorded
/// once the host and everything in its process group have exited. A host no owner of this daemon
/// holds is taken over only while its node still has something to do; one whose task owes nothing
/// more is ended where it is, and its Claude session is never resumed.
#[allow(clippy::too_many_arguments)]
async fn close_sdk_node(
    domain: &dyn Runtime, timing: &Timing, now: i64, state: &DomainState, task: &Task, agent: &AgentId, kind: Due,
    row: TaskClosing, owned: bool,
) -> Result<()> {
    let cleanup = matches!(kind, Due::Cleanup);
    let user_closed = state.post_merge_closed_by_user(&task.id);
    let folder_gone = crate::sdk_owner::closing_without_folder(domain, state, &task.id);
    let owes_nothing = !cleanup || task.wrap_up.cleaned_at.is_some() || user_closed || folder_gone;
    let cleaned_at = task.wrap_up.cleaned_at.map(|revision| revision as i64);
    let confirmed = cleaned_at.is_some() && cleaned_at > row.attempted_cleaned_at;
    if !owned && !owes_nothing && sdk_working(state, task) {
        // Its lifecycle still says it works, but no host of this daemon runs it: the facts of what
        // became of that host are in its journal, which the take-over commits.
        if now < row.next_attempt {
            return Ok(());
        }
        return match take_over(domain, task) {
            Ok(()) => Ok(()),
            Err(why) => {
                // Nothing runs the node to read a message, and one it cannot take would keep its
                // lifecycle at work: the failure is only counted.
                let why = format!("no host could take over its session: {why}");
                let row = retry_later(timing, now, task, row, &why);
                count_failure(domain, task, row, CloseStop::Error, &why, cleanup)
            }
        };
    }
    if cleanup {
        let working = sdk_working(state, task);
        if state.post_merge_owed(task) && !user_closed {
            forget_post_merge_check(domain, &task.id)?;
            return hold_while_working_after_merge(domain, now, row);
        } else if !folder_gone && working && task.wrap_up.cleaned_at.is_none() && !user_closed {
            return hold_while_working_after_merge(domain, now, row);
        } else if !folder_gone && task.wrap_up.cleaned_at.is_none() && !user_closed
            && task.wrap_up.cleanup_turn != Some(alondra_domain_model::CleanupTurn::Rested) && !state.cleanup_held_by_alondra(task) {
            // The node was asked to clean up: it closes once it says it did, once a turn of it
            // after the request ended at rest, or while Alondra's own pause keeps the request from
            // it, whatever time any of that took.
            return Ok(());
        }
    }
    if now < row.next_attempt && !confirmed {
        return Ok(());
    }
    let left = if task.cancelled() { vec![] } else { domain.engine().worktree_leftovers(task).await };
    if !left.is_empty() {
        if !owned && let Err(why) = take_over(domain, task) {
            log!("no host could resume the session of {} to settle its worktrees: {why}", task.id);
        }
        return hold_for_worktrees(domain, timing, now, task, agent, row, &left, cleanup);
    }
    forget_post_merge_check(domain, &task.id)?;
    if !domain.store().lock_or_exit("the domain store").claim_task_removal(&task.id, now)? {
        return Ok(());
    }
    if !owned {
        return match end_unowned_host(domain, task) {
            Ok(()) => finish_with_worktree(domain, timing, now, task, agent, row, cleanup, NodeEnd::Gone).await,
            Err(why) => {
                let why = format!("its host could not be ended: {why}");
                let row = retry_later(timing, now, task, row, &why);
                count_failure(domain, task, row, CloseStop::Error, &why, cleanup)
            }
        };
    }
    match blocking(|| domain.close_sdk_host(agent)) {
        Ok(()) => finish_with_worktree(domain, timing, now, task, agent, row, cleanup, NodeEnd::Closed).await,
        Err(why) => {
            let why = why.to_string();
            failure(domain, timing, now, task, agent, row, &why, (CloseStop::Error, &why), cleanup)
        }
    }
}

/// Ends the host no owner of this daemon holds, without resuming its session, once it was ever
/// started: one that never was has nothing to end.
fn end_unowned_host(domain: &dyn Runtime, task: &Task) -> Result<(), String> {
    let Some((backend, runtime)) = domain.sdk() else { return Ok(()) };
    if !crate::sdk_owner::host_started(backend, domain, &task.id) {
        return Ok(());
    }
    let backend = backend.clone();
    blocking(|| crate::sdk_owner::end_unowned_host(runtime, &backend, task.id.clone())).map_err(|crate::sdk_owner::Failed { why, .. }| why)
}

/// No owner of this daemon holds the host of a closing SDK task: it takes the execution as a daemon
/// that comes back does (`adopt`). A host that still runs is adopted; one that ended leaves the
/// facts of its end in its journal, which are committed, and a host that resumes the same Claude
/// session takes its place, so the node can still be told to settle its worktrees. Nothing is
/// guessed about a host this daemon cannot see.
fn take_over(domain: &dyn Runtime, task: &Task) -> Result<(), String> {
    let (backend, runtime) = domain.sdk().ok_or("this daemon runs no Agent SDK host")?;
    let backend = backend.clone();
    let plugin = domain.ports().plugins().node(domain.home());
    blocking(|| crate::sdk_owner::take_over_unowned(&runtime, &backend, &task.id, plugin.as_deref()))
        .map_err(|crate::sdk_owner::Failed { why, .. }| why)
}


/// Whether the lifecycle of an SDK task shows its execution still at work: a turn, background
/// work it owns, a continuation it owes or an input it has not taken yet.
fn sdk_working(state: &DomainState, task: &Task) -> bool {
    use alondra_domain::ActivityView;
    state.lifecycle_of(&task.id).ok()
        .is_some_and(|lifecycle| matches!(lifecycle.activity(),
            ActivityView::Starting | ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation))
}

/// The session is gone; the worktree Alondra made for the task goes too, which `claude rm` leaves
/// because Claude Code did not make it. The task finishes only once it is gone, so a worktree with
/// work not saved is a failed close and is tried again, never forgotten. Nothing is deleted while
/// the domain would refuse the end of the node: a task that stays open keeps its folder.
#[allow(clippy::too_many_arguments)]
async fn finish_with_worktree(
    domain: &dyn Runtime, timing: &Timing, now: i64, task: &Task, agent: &AgentId, row: TaskClosing, cleanup: bool, end: NodeEnd,
) -> Result<()> {
    if task.cancelled() {
        domain.engine().release_cancelled_worktrees(task).await?;
        return finish(domain, &row, Some(agent), end);
    }
    if let Some(reason) = refused_end(domain, task, agent, end) {
        return failure(domain, timing, now, task, agent, row, &reason, (CloseStop::Error, &reason), cleanup);
    }
    let left = domain.engine().release_worktrees(task).await?;
    if !left.is_empty() {
        let detail: Vec<String> = left.iter().map(|(path, what)| format!("{path}: {what}")).collect();
        let reason = format!("its session is closed, but worktrees of the task hold work that is not saved: {}", detail.join("; "));
        return failure(domain, timing, now, task, agent, row, &reason, (CloseStop::Unsaved, &detail.join("\n")), cleanup);
    }
    let dir = domain.task_worktree(&task.id)?.filter(|dir| !task.worktrees.contains_key(dir));
    if let Some(dir) = &dir { domain.engine().worktree_ready(dir).await?; }
    let Some(dir) = dir.filter(|dir| domain.engine().may_handle_worktree(dir).is_ok() && domain.engine().worktree_heir(task, dir).is_ok_and(|heir| heir.is_none())) else {
        return finish(domain, &row, Some(agent), end);
    };
    match domain.ports().worktrees().release_finished_worktree(Path::new(&dir), Some(&alondra_engine_ports::worktrees::slug(&task.id.0)), false, domain.state().merged_pr(task).is_some()).await {
        Ok(TaskWorktree::Unsaved(left)) => {
            let reason = format!("its session is closed, but its worktree has work that is not saved: {left}");
            failure(domain, timing, now, task, agent, row, &reason, (CloseStop::Unsaved, &left), cleanup)
        }
        Ok(TaskWorktree::Unavailable(why)) => Err(Unreachable(why).into()),
        Ok(released) => {
            if let TaskWorktree::Removed { freed } = released {
                log!("deleting the worktree {dir} freed {}", alondra_core::machine_incident::bytes_text(freed, alondra_view_model::Lang::En));
            }
            finish(domain, &row, Some(agent), end)
        }
        Err(why) => failure(domain, timing, now, task, agent, row, &why, (CloseStop::Error, &why), cleanup),
    }
}

/// Why the domain would refuse to record `end` for the node of `task` once its worktrees are
/// released, found on a copy of the state, before anything is deleted.
fn refused_end(domain: &dyn Runtime, task: &Task, agent: &AgentId, end: NodeEnd) -> Option<String> {
    let mut probe = domain.state().as_ref().clone();
    for worktree in probe.tasks.get_mut(&task.id)?.worktrees.values_mut().filter(|worktree| worktree.end.is_none()) {
        worktree.end = Some(alondra_domain_model::WorktreeEnd::Removed);
    }
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: probe.data_epoch.clone(), command_id: "probe-node-end".into(),
        expected_revision: probe.revision,
        command: Command::RecordNodeEnd { task_id: task.id.clone(), end, agent_id: Some(agent.clone()) },
    };
    probe.execute(&Actor::System, &envelope).err().map(|error| format!("the domain refuses to end its node: {}", error.message))
}

/// A check an earlier daemon kept on the work after the merge, with the card it may have raised:
/// gone, as no clock asks about that work any more.
fn forget_post_merge_check(domain: &dyn Runtime, task_id: &TaskId) -> Result<()> {
    let check = domain.store().lock_or_exit("the domain store").post_merge_check(task_id)?;
    let Some(check) = check else { return Ok(()) };
    if let Some(card) = &check.problem_id {
        let _ = domain.withdraw_problem(&InteractionId(card.clone()));
    }
    domain.store().lock_or_exit("the domain store").forget_post_merge_check(task_id)
}

/// The node said it still works on something after the merge: its margin starts over from now
/// on every pass, so it runs again in full once the node says it is done.
fn hold_while_working_after_merge(domain: &dyn Runtime, now: i64, mut row: TaskClosing) -> Result<()> {
    row.since = now;
    row.ready_since = None;
    row.failures = 0;
    row.last_error = None;
    row.next_attempt = 0;
    domain.store().lock_or_exit("the domain store").save_task_closing(&row)
}

#[allow(clippy::too_many_arguments)]
fn failure(
    domain: &dyn Runtime, timing: &Timing, now: i64, task: &Task, agent: &AgentId, row: TaskClosing, reason: &str,
    (stop, detail): (CloseStop, &str), cleanup: bool,
) -> Result<()> {
    let told = already_told(&row, task, reason);
    let row = retry_later(timing, now, task, row, reason);
    if cleanup && !told {
        let text = close_failed_text(reason, &task.id, timing.retry.as_secs() / 60);
        { let state = domain.state();
            tell_node(domain, &state, task, agent, &text);
        }
    }
    count_failure(domain, task, row, stop, detail, cleanup)
}

/// The node was already told `reason` on the last attempt and has not confirmed its cleanup since:
/// nothing changed, so it is not told again.
fn already_told(row: &TaskClosing, task: &Task, reason: &str) -> bool {
    row.last_error.as_deref() == Some(reason) && row.attempted_cleaned_at == task.wrap_up.cleaned_at.map(|revision| revision as i64)
}

fn retry_later(timing: &Timing, now: i64, task: &Task, mut row: TaskClosing, reason: &str) -> TaskClosing {
    row.failures += 1;
    row.last_error = Some(reason.to_owned());
    row.next_attempt = now + millis(timing.retry);
    row.attempted_cleaned_at = task.wrap_up.cleaned_at.map(|revision| revision as i64);
    row.rm_started_at = None;
    row
}

/// A worktree of the task is out of reach, as on a volume that is not mounted: the close waits for
/// it without counting a failure, so no card is raised, and tries again later.
fn wait_for_folder(domain: &dyn Runtime, timing: &Timing, now: i64, task: &Task, mut row: TaskClosing, reason: &str) -> Result<()> {
    log!("{reason}");
    row.last_error = Some(reason.to_owned());
    row.next_attempt = now + millis(timing.retry);
    row.attempted_cleaned_at = task.wrap_up.cleaned_at.map(|revision| revision as i64);
    row.rm_started_at = None;
    domain.store().lock_or_exit("the domain store").save_task_closing(&row)
}

/// The task cannot close while one of its worktrees holds work no remote has: its node, which
/// keeps running, is told which and how to settle each, and the close is tried again later.
#[allow(clippy::too_many_arguments)]
fn hold_for_worktrees(
    domain: &dyn Runtime, timing: &Timing, now: i64, task: &Task, agent: &AgentId, row: TaskClosing, left: &[(String, String)], cleanup: bool,
) -> Result<()> {
    let detail: Vec<String> = left.iter().map(|(path, what)| format!("{path}: {what}")).collect();
    let reason = format!("worktrees of the task hold work that is not saved: {}", detail.join("; "));
    let told = already_told(&row, task, &reason);
    let row = retry_later(timing, now, task, row, &reason);
    if !told {
        let state = domain.state();
        tell_node(domain, &state, task, agent, &alondra_engine_ports::worktrees::settle_text(&task.id, left));
    }
    count_failure(domain, task, row, CloseStop::Unsaved, &detail.join("\n"), cleanup)
}

fn count_failure(domain: &dyn Runtime, task: &Task, mut row: TaskClosing, stop: CloseStop, detail: &str, cleanup: bool) -> Result<()> {
    if row.failures >= MAX_FAILURES && row.problem_id.is_none() {
        row.problem_id = raise_card(domain, task, stop, detail, cleanup);
    }
    domain.store().lock_or_exit("the domain store").save_task_closing(&row)?;
    Ok(())
}

/// Tells the task's node `text`: through its lifecycle when it runs through the Agent SDK, down
/// its connection otherwise.
fn tell_node(domain: &dyn Runtime, state: &DomainState, task: &Task, agent: &AgentId, text: &str) {
    if crate::sdk_owner::runs_through_sdk(state, agent) {
        let id = format!("closer-{}-{}", task.id, alondra_foundation::random_hex(8));
        if let Err(error) = domain.engine().tell_node(&task.id, text, &id) {
            log!("could not tell the node of {} why its task does not close: {error}", task.id);
        }
    } else {
        domain.engine().tell_session(&agent.0, text, false);
    }
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

fn raise_card(domain: &dyn Runtime, task: &Task, stop: CloseStop, detail: &str, cleanup: bool) -> Option<String> {
    let id = format!("{CLOSE_PROBLEM_PREFIX}{}-{}", task.id, alondra_foundation::random_hex(6));
    let scope = if cleanup {
        Scope::Task { task_id: task.id.clone() }
    } else {
        Scope::Orchestrator { request_id: Some(task.request_id.clone()) }
    };
    let card = CloseProblem { title: task.title.clone(), cancelled: !cleanup, stop, detail: detail.trim().to_owned() };
    let (need, why) = card.text(Lang::En);
    let (options, recommended) = card.options();
    match domain.raise_worded_problem_in(scope, &id, Some(SystemCard::CloseProblem), &format!("close-session:{}", task.id), need, why, card.to_body(), options, Some(recommended)) {
        Ok(_) => Some(id),
        Err(error) => {
            log!("could not tell the user Alondra cannot close the session of {}: {error:?}", task.id);
            None
        }
    }
}

#[cfg(test)]
#[cfg(feature = "whole-engine")]
#[cfg(not(feature = "real-hosts"))]
mod tests;

/// Settles the tasks a commit left due with nobody to close them.
pub struct Unattended;

impl crate::domain_runtime::CommitObserver for Unattended {
    fn changed(&self, domain: &dyn Runtime, state: &DomainState) {
        settle_unattended(domain, state);
    }
}
