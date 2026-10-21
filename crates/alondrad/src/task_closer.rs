//! Closing the node of a task that is finished or cancelled: Alondra asks the node to clean up,
//! waits for it to say so (or for the wait to run out), closes its session with `claude rm` and
//! records what became of it, which is what makes a closing task done. A node that still works
//! once its margin is over (at once for a cancelled task) has its turn cut, unless it declared work
//! after the merge; a close that keeps failing ends in a card the user answers. Nothing is closed
//! while the node waits for the user.
use crate::domain_github::execute_system;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{PostMergeCheck, TaskClosing};
use crate::node_close::{CloseOutcome, Sessions, close_node};
use crate::worktree::TaskWorktree;
use alondra_core::close_problem::{CLOSE_PROBLEM_PREFIX, CloseProblem, CloseStop};
use alondra_core::views::Lang;
use alondra_core::domain::{
    AgentId, CardSpec, Command, DomainState, InteractionId, InteractionPayload, NodeEnd, POST_MERGE_CLOSE, POST_MERGE_LEAVE,
    POST_MERGE_PROBLEM_PREFIX, PostMergeWork, QuestionOption, Scope, Task, TaskId, TaskState, clip,
};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub(crate) const RETRY: Duration = Duration::from_secs(5 * 60);
pub(crate) const TICK: Duration = Duration::from_secs(30);
const MAX_FAILURES: u32 = 3;
const BUSY_FALLBACK: Duration = Duration::from_secs(30 * 60);
const STILL_WORKING: &str = "it is still working";
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

    /// A sandbox with fast timing looks again every couple of seconds.
    pub(crate) fn sandbox(wait: Option<Duration>) -> Self {
        Self { wait, retry: Duration::from_secs(10), tick: Duration::from_secs(2), post_merge_ask: POST_MERGE_ASK, post_merge_answer: POST_MERGE_ANSWER }
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
pub(crate) fn settle_unattended(domain: &DomainRuntime, state: &DomainState) {
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
            let mut store = domain.store.lock().expect("domain store poisoned");
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
async fn finish_unattended(domain: &DomainRuntime, task: &Task) -> Result<()> {
    let left = crate::task_worktrees::release_all(domain, task).await?;
    if !left.is_empty() {
        let list: Vec<String> = left.iter().map(|(path, what)| format!("{path}: {what}")).collect();
        anyhow::bail!("the worktrees of {} hold work that is not saved: {}", task.id, list.join("; "));
    }
    if let Some(dir) = domain.task_worktree(&task.id)?.filter(|dir| !task.worktrees.contains_key(dir)) {
        match crate::worktree::release_task_worktree(Path::new(&dir), &crate::launch_worktree::slug(&task.id.0), true).await {
            Ok(TaskWorktree::Unsaved(left)) => anyhow::bail!("the worktree of {} has work that is not saved: {left}", task.id),
            Ok(_) => {}
            Err(why) => anyhow::bail!("the worktree of {} could not be released: {why}", task.id),
        }
    }
    {
        let mut store = domain.store.lock().expect("domain store poisoned");
        store.dequeue_launch(&task.id)?;
        store.forget_auto_launch(&task.id)?;
    }
    execute_system(domain, Command::RecordTaskCleanup { task_id: task.id.clone() }).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(())
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
    let mut failed = Vec::new();
    for (task_id, kind) in due_tasks {
        if let Err(error) = close_one(domain, sessions, timing, now, &task_id, kind).await {
            failed.push(format!("{task_id}: {error:#}"));
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
/// released like one whose node the user closed: what a relaunch needs is kept, the orchestrator
/// is told, and the changes asked for reach the node that replaces it.
fn record_end(domain: &DomainRuntime, task_id: &TaskId, end: NodeEnd, agent: Option<&AgentId>) {
    let before = domain.state().ok();
    let back_to_work = matches!(end, NodeEnd::Closed | NodeEnd::Gone)
        && before.as_ref().and_then(|state| state.tasks.get(task_id)).is_some_and(|task| !task.over() && task.wrap_up.asked_at.is_none());
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
    if let Some(agent) = agent {
        domain.live.set_close_interrupted(&agent.0, None);
    }
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
    if matches!(kind, Due::Unattended) {
        return finish_unattended(domain, task).await;
    }
    let mut row = domain.store.lock().expect("domain store poisoned").begin_task_closing(task_id, now)?;
    let Some(agent) = task.agent_id.clone() else {
        return finish(domain, &row, None, NodeEnd::Gone);
    };
    let owned = domain.owns_sdk_host(&agent);
    if owned || crate::sdk_owner::runs_through_sdk(&state, &agent) {
        return close_sdk_node(domain, timing, now, &state, task, &agent, kind, row, owned).await;
    }
    let orchestrator = domain.live.node(&agent.0).is_some_and(|node| node.is_orchestrator);
    if orchestrator || !domain.launched_by_alondra(&agent)? {
        return finish(domain, &row, Some(&agent), NodeEnd::UsersOwn);
    }
    if let Some(card_id) = row.problem_id.clone() {
        match card(&state, &card_id) {
            Card::Leave => return finish(domain, &row, Some(&agent), NodeEnd::KeptByUser),
            Card::Retry | Card::Withdrawn => {
                domain.live.set_close_interrupted(&agent.0, None);
                row.failures = 0;
                row.problem_id = None;
                row.next_attempt = 0;
                domain.store.lock().expect("domain store poisoned").save_task_closing(&row)?;
            }
            Card::Unanswered => return Ok(()),
        }
    }
    let session_over = state.node_session_closed(task);
    let mut close_now = session_over;
    match (&kind, state.post_merge_owed(task), &task.wrap_up.post_merge_work) {
        (Due::Cleanup, true, Some(work)) if !session_over => match watch_post_merge_work(
            domain, &state, timing, now, task, &agent, work,
            domain.live.node(&agent.0).is_some_and(|node| node.busy),
            Some(&|text: &str| { let _ = crate::sdk_handover::tell(domain, &agent.0, crate::node_push::ALONDRA, text, false); }),
        )? {
            Watch::Hold => return hold_while_working_after_merge(domain, now, row),
            Watch::CloseNow => close_now = true,
        },
        (Due::Cleanup, true, None) if !session_over => return hold_while_working_after_merge(domain, now, row),
        _ => {
            close_now |= matches!(post_merge_answer(domain, &state, task_id)?, Some(Card::Retry));
            forget_post_merge_check(domain, task_id)?;
        }
    }
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
    let left = crate::task_worktrees::leftovers(task).await;
    if !left.is_empty() {
        return hold_for_worktrees(domain, timing, now, task, &agent, row, &left, matches!(kind, Due::Cleanup));
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
        let margin = match kind {
            Due::Cancelled | Due::Unattended => 0,
            Due::Cleanup => millis(timing.wait.unwrap_or(BUSY_FALLBACK)),
        };
        if now - waiting_since < margin {
            return Ok(());
        }
        return match domain.live.close_interrupted_at(&agent.0) {
            None => {
                let _ = domain.live.pushes.interrupt(&agent.0);
                domain.live.set_close_interrupted(&agent.0, Some(now));
                Ok(())
            }
            Some(at) if now - at < millis(timing.retry) => Ok(()),
            Some(_) => {
                domain.live.set_close_interrupted(&agent.0, None);
                failure(domain, timing, now, task, &agent, row, STILL_WORKING, (CloseStop::Working, ""), matches!(kind, Due::Cleanup))
            }
        };
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
    let cleanup = matches!(kind, Due::Cleanup);
    match outcome {
        CloseOutcome::Closed { .. } => finish_with_worktree(domain, timing, now, task, &agent, row, cleanup, NodeEnd::Closed).await,
        CloseOutcome::NotListed => {
            crate::node_revival::user_closed(domain, &agent.0)?;
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
    domain: &DomainRuntime, timing: &Timing, now: i64, state: &DomainState, task: &Task, agent: &AgentId, kind: Due,
    row: TaskClosing, owned: bool,
) -> Result<()> {
    let cleanup = matches!(kind, Due::Cleanup);
    let user_closed = matches!(post_merge_answer(domain, state, &task.id)?, Some(Card::Retry));
    let owes_nothing = !cleanup || task.wrap_up.cleaned_at.is_some() || user_closed;
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
            let Some(work) = &task.wrap_up.post_merge_work else {
                return hold_while_working_after_merge(domain, now, row);
            };
            let ask = |text: &str| tell_node(domain, state, task, agent, text);
            if let Watch::Hold = watch_post_merge_work(domain, state, timing, now, task, agent, work, working, Some(&ask))? {
                return hold_while_working_after_merge(domain, now, row);
            }
        } else if working && task.wrap_up.cleaned_at.is_none() && !user_closed {
            return hold_while_working_after_merge(domain, now, row);
        } else if task.wrap_up.cleaned_at.is_none() && !user_closed && !timing.wait.is_some_and(|wait| now >= row.since + millis(wait)) {
            // The node was asked to clean up: it closes once it says it did, or after the same
            // wait as a node of `claude --bg`.
            return Ok(());
        }
    }
    if now < row.next_attempt && !confirmed {
        return Ok(());
    }
    let left = crate::task_worktrees::leftovers(task).await;
    if !left.is_empty() {
        if !owned && let Err(why) = take_over(domain, task) {
            log!("no host could resume the session of {} to settle its worktrees: {why}", task.id);
        }
        return hold_for_worktrees(domain, timing, now, task, agent, row, &left, cleanup);
    }
    forget_post_merge_check(domain, &task.id)?;
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
fn end_unowned_host(domain: &DomainRuntime, task: &Task) -> Result<(), String> {
    let Some((backend, runtime)) = domain.sdk() else { return Ok(()) };
    if !crate::sdk_owner::host_started(backend, domain, &task.id) {
        return Ok(());
    }
    let backend = backend.clone();
    blocking(|| crate::sdk_owner::end_unowned_host(runtime, &backend, task.id.clone())).map_err(|crate::sdk_owner::Failed(why)| why)
}

/// No owner of this daemon holds the host of a closing SDK task: it takes the execution as a daemon
/// that comes back does (`adopt`). A host that still runs is adopted; one that ended leaves the
/// facts of its end in its journal, which are committed, and a host that resumes the same Claude
/// session takes its place, so the node can still be told to settle its worktrees. Nothing is
/// guessed about a host this daemon cannot see.
fn take_over(domain: &DomainRuntime, task: &Task) -> Result<(), String> {
    let (backend, runtime) = domain.sdk().ok_or("this daemon runs no Agent SDK host")?;
    let backend = backend.clone();
    let plugin = crate::plugins::node_in(domain.home());
    let driver = blocking(|| crate::sdk_owner::adopt(Arc::clone(&runtime), &backend, task.id.clone(), plugin.as_deref()))
        .map_err(|crate::sdk_owner::Failed(why)| why)?;
    domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));
    Ok(())
}

/// Runs `wait`, which waits for processes to exit, without stalling the runtime's other tasks.
pub(crate) fn blocking<T>(wait: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(wait),
        _ => wait(),
    }
}

/// Whether the lifecycle of an SDK task shows its execution still at work: a turn, background
/// work it owns, a continuation it owes or an input it has not taken yet.
fn sdk_working(state: &DomainState, task: &Task) -> bool {
    use alondra_domain::ActivityView;
    state.lifecycle_of(&task.id).ok()
        .is_some_and(|lifecycle| matches!(lifecycle.activity(),
            ActivityView::Starting | ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation))
}

/// What the user answered on the card about the task's work after the merge, if it was raised.
fn post_merge_answer(domain: &DomainRuntime, state: &DomainState, task: &TaskId) -> Result<Option<Card>> {
    let check = domain.store.lock().expect("domain store poisoned").post_merge_check(task)?;
    Ok(check.and_then(|check| check.problem_id).map(|id| card(state, &id)))
}

/// The session is gone; the worktree Alondra made for the task goes too, which `claude rm` leaves
/// because Claude Code did not make it. The task finishes only once it is gone, so a worktree with
/// work not saved is a failed close and is tried again, never forgotten.
#[allow(clippy::too_many_arguments)]
async fn finish_with_worktree(
    domain: &DomainRuntime, timing: &Timing, now: i64, task: &Task, agent: &AgentId, row: TaskClosing, cleanup: bool, end: NodeEnd,
) -> Result<()> {
    let left = crate::task_worktrees::release_all(domain, task).await?;
    if !left.is_empty() {
        let detail: Vec<String> = left.iter().map(|(path, what)| format!("{path}: {what}")).collect();
        let reason = format!("its session is closed, but worktrees of the task hold work that is not saved: {}", detail.join("; "));
        return failure(domain, timing, now, task, agent, row, &reason, (CloseStop::Unsaved, &detail.join("\n")), cleanup);
    }
    let Some(dir) = domain.task_worktree(&task.id)?.filter(|dir| !task.worktrees.contains_key(dir)) else {
        return finish(domain, &row, Some(agent), end);
    };
    match crate::worktree::release_task_worktree(Path::new(&dir), &crate::launch_worktree::slug(&task.id.0), true).await {
        Ok(TaskWorktree::Unsaved(left)) => {
            let reason = format!("its session is closed, but its worktree has work that is not saved: {left}");
            failure(domain, timing, now, task, agent, row, &reason, (CloseStop::Unsaved, &left), cleanup)
        }
        Ok(_) => finish(domain, &row, Some(agent), end),
        Err(why) => failure(domain, timing, now, task, agent, row, &why, (CloseStop::Error, &why), cleanup),
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
#[allow(clippy::too_many_arguments)]
fn watch_post_merge_work(
    domain: &DomainRuntime, state: &DomainState, timing: &Timing, now: i64, task: &Task, agent: &AgentId, work: &PostMergeWork,
    working: bool, ask: Option<&dyn Fn(&str)>,
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
        if !working && now - asked >= millis(timing.post_merge_answer) {
            check.problem_id = raise_post_merge_card(domain, task, &work.reason, now - check.since);
        }
    } else if now - check.since >= millis(timing.post_merge_ask) {
        let text = still_working_text(&work.reason, &task.id, now - check.since, timing.post_merge_answer);
        match ask {
            Some(ask) => ask(&text),
            None => log!("{agent} runs through the Agent SDK and cannot be asked yet whether its work after the merge still runs"),
        }
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
    let id = format!("{POST_MERGE_PROBLEM_PREFIX}{}-{}", task.id, crate::auth::random_hex(6));
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
        QuestionOption { id: POST_MERGE_CLOSE.into(), label: "Close the node".into(), description: None },
        QuestionOption { id: POST_MERGE_LEAVE.into(), label: "Leave it".into(), description: None },
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
fn failure(
    domain: &DomainRuntime, timing: &Timing, now: i64, task: &Task, agent: &AgentId, row: TaskClosing, reason: &str,
    (stop, detail): (CloseStop, &str), cleanup: bool,
) -> Result<()> {
    let told = already_told(&row, task, reason);
    let row = retry_later(timing, now, task, row, reason);
    if cleanup && !told {
        let text = close_failed_text(reason, &task.id, timing.retry.as_secs() / 60);
        if let Ok(state) = domain.state() {
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

/// The task cannot close while one of its worktrees holds work no remote has: its node, which
/// keeps running, is told which and how to settle each, and the close is tried again later.
#[allow(clippy::too_many_arguments)]
fn hold_for_worktrees(
    domain: &DomainRuntime, timing: &Timing, now: i64, task: &Task, agent: &AgentId, row: TaskClosing, left: &[(String, String)], cleanup: bool,
) -> Result<()> {
    let detail: Vec<String> = left.iter().map(|(path, what)| format!("{path}: {what}")).collect();
    let reason = format!("worktrees of the task hold work that is not saved: {}", detail.join("; "));
    let told = already_told(&row, task, &reason);
    let row = retry_later(timing, now, task, row, &reason);
    if !told {
        let state = domain.state()?;
        tell_node(domain, &state, task, agent, &crate::task_worktrees::settle_text(&task.id, left));
    }
    count_failure(domain, task, row, CloseStop::Unsaved, &detail.join("\n"), cleanup)
}

fn count_failure(domain: &DomainRuntime, task: &Task, mut row: TaskClosing, stop: CloseStop, detail: &str, cleanup: bool) -> Result<()> {
    if row.failures >= MAX_FAILURES && row.problem_id.is_none() {
        row.problem_id = raise_card(domain, task, stop, detail, cleanup);
    }
    domain.store.lock().expect("domain store poisoned").save_task_closing(&row)?;
    Ok(())
}

/// Tells the task's node `text`: through its lifecycle when it runs through the Agent SDK, down
/// its connection otherwise.
fn tell_node(domain: &DomainRuntime, state: &DomainState, task: &Task, agent: &AgentId, text: &str) {
    if crate::sdk_owner::runs_through_sdk(state, agent) {
        let id = format!("closer-{}-{}", task.id, crate::auth::random_hex(8));
        if let Err(error) = crate::domain_messages::tell_sdk_node(domain, &task.id, crate::node_push::ALONDRA, text, &id) {
            log!("could not tell the node of {} why its task does not close: {error}", task.id);
        }
    } else {
        let _ = crate::sdk_handover::tell(domain, &agent.0, crate::node_push::ALONDRA, text, false);
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

fn raise_card(domain: &DomainRuntime, task: &Task, stop: CloseStop, detail: &str, cleanup: bool) -> Option<String> {
    let id = format!("{CLOSE_PROBLEM_PREFIX}{}-{}", task.id, crate::auth::random_hex(6));
    let scope = if cleanup {
        Scope::Task { task_id: task.id.clone() }
    } else {
        Scope::Orchestrator { request_id: Some(task.request_id.clone()) }
    };
    let card = CloseProblem { title: task.title.clone(), cancelled: !cleanup, stop, detail: detail.trim().to_owned() };
    let (need, why) = card.text(Lang::En);
    let (options, recommended) = card.options();
    match domain.raise_worded_problem_in(scope, &id, &format!("close-session:{}", task.id), need, why, card.to_body(), options, Some(recommended)) {
        Ok(_) => Some(id),
        Err(error) => {
            log!("could not tell the user Alondra cannot close the session of {}: {error:?}", task.id);
            None
        }
    }
}

#[cfg(test)]
mod tests;
