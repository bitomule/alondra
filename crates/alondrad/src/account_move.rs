//! Reconciles historical account-move journals without selecting another account for a started
//! node. Pre-admission moves are undone; an already committed destination is recovered only
//! after positive evidence that its native execution started. The old journal format stays readable.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{AccountMove, AccountMovePlan, AccountMoveStep};
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, Policy, Provider};
use alondra_core::domain::*;
use std::sync::Arc;
use std::time::Duration;

/// How often the worker looks again when nothing in the domain changes: an account that can take
/// work again, or a move that waited.
const EVERY: Duration = Duration::from_secs(10);
/// How long a task stays on the account it moved to when the limit it left said no reset: one
/// session window.
const NO_RESET: Millis = 5 * 60 * 60 * 1000;

/// The daemon's moves: one advances at a time.
#[derive(Default)]
pub(crate) struct Moves {
    moving: tokio::sync::Mutex<()>,
    /// A move stops right after recording this step, as a daemon that dies then does: the
    /// admission dies after its commit, before its host starts.
    #[cfg(test)]
    pub(crate) crash_after: std::sync::Mutex<Option<AccountMoveStep>>,
}

fn enabled(_domain: &DomainRuntime) -> bool {
    false
}

fn policy(domain: &DomainRuntime) -> Policy {
    crate::accounts::config(domain.home()).map(|config| config.distribution.policy).unwrap_or_default()
}

fn moves(domain: &DomainRuntime) -> Vec<AccountMove> {
    match domain.store.lock_or_exit("the domain store").account_moves() {
        Ok(moves) => moves,
        Err(error) => {
            log!("could not read the journal of account moves: {error:#}");
            vec![]
        }
    }
}

fn latest(domain: &DomainRuntime, task: &TaskId) -> Option<AccountMove> {
    moves(domain).into_iter().rev().find(|moving| moving.task == *task)
}

/// Whether a move holds the task's execution: planned and not yet admitted, so its old host is
/// stopped or about to be, and nothing but the move takes the execution over.
pub(crate) fn holds_execution(domain: &DomainRuntime, task: &TaskId) -> bool {
    latest(domain, task).is_some_and(|moving| moving.step < AccountMoveStep::Admitted)
}

/// Whether `account` can take work at `now`: on, signed in as far as Alondra knows, paid by
/// subscription and with neither its session nor its week used up.
fn can_take(state: &DomainState, account: &AccountId, now: Millis) -> bool {
    let Some(status) = state.accounts.iter().find(|status| status.provider == Provider::Claude && status.id == *account) else { return false };
    let day = crate::node_reports::local_day(now);
    alondra_core::usage::choose_account(Policy::Chain, Provider::Claude, std::slice::from_ref(status), &state.account_usage, state.account_reserve, now, &day).as_ref() == Some(account)
}

/// The account a task that leaves `from` goes on with: one that can take work, the one the
/// user's distribution prefers among them.
fn destination(state: &DomainState, policy: Policy, from: &AccountId, now: Millis) -> Option<AccountId> {
    let others: Vec<_> = state.accounts.iter().filter(|status| status.id != *from).cloned().collect();
    if others.is_empty() {
        return None;
    }
    let day = crate::node_reports::local_day(now);
    alondra_core::usage::choose_account(policy, Provider::Claude, &others, &state.account_usage, state.account_reserve, now, &day)
        .filter(|to| to != from && can_take(state, to, now))
}

/// When the limit the task leaves resets: what Claude said with it, else what the account's
/// usage says, else one session window from now.
fn resets(state: &DomainState, held: &ClaudeHeld, now: Millis) -> Millis {
    held.resets_at
        .and_then(|seconds| Millis::try_from(seconds).ok()?.checked_mul(1000))
        .filter(|at| *at > now)
        .or_else(|| state.account_usage.iter().find(|usage| usage.account == held.account).and_then(|usage| usage.reset_after_limit(now)))
        .unwrap_or(now + NO_RESET)
}

/// Records the plan of moving the held task to `to`: the session its execution runs, which the
/// execution on `to` resumes in the same folder.
fn plan(domain: &DomainRuntime, held: &ClaudeHeld, to: AccountId, now: Millis) -> anyhow::Result<i64> {
    use anyhow::Context as _;
    let state = domain.state()?;
    let node = state.tasks.get(&held.task).and_then(|task| task.agent_id.clone()).context("the task has no node")?;
    let lifecycle = state.lifecycle_of(&held.task).map_err(|error| anyhow::anyhow!(error.message))?;
    let stamp = lifecycle.execution().context("the task has no execution")?;
    anyhow::ensure!(crate::accounts::share_sessions(domain, &held.account, &to),
        "account {to} does not read the conversations of account {}, so the session would not resume there", held.account);
    let mut store = domain.store.lock_or_exit("the domain store");
    let metadata = store.conversation_metadata(&held.task, stamp.conversation().as_str())?.context("its Claude session is not known")?;
    let session = alondra_provider_claude::conversation_session(&metadata).context("its Claude session is not known")?;
    let cwd = store.task_worktree(&held.task)?.context("the folder its session works in is not known")?;
    let plan = AccountMovePlan {
        from: held.account.clone(), to, card: held.card.clone(), node, instance: stamp.instance().as_str().to_owned(), session, cwd,
        until: resets(&state, held, now),
    };
    let number = store.plan_account_move(&held.task, &plan)?;
    log!("{} reached the usage limit of account {}: it goes on with account {} in the same Claude session", held.task, plan.from, plan.to);
    Ok(number)
}

/// Why the planned move can no longer happen, if it cannot.
fn refused(_domain: &DomainRuntime, _moving: &AccountMove) -> Option<String> {
    Some("Accounts and providers are chosen when creating a node. This execution stays on its original account".into())
}

/// A persisted lock or a preallocated Claude UUID does not prove that the destination native
/// process ever ran. Available is committed only after native init or positive native consumption.
pub(crate) fn require_started_destination(domain:&DomainRuntime,task:&TaskId)->Result<(),String>{
    let moving=domain.store.lock_or_exit("the domain store").account_moves().map_err(|error|format!("reading historical account moves: {error:#}"))?
        .into_iter().rev().find(|moving|moving.task==*task);
    let Some(moving)=moving.filter(|moving|matches!(moving.step,AccountMoveStep::Admitted|AccountMoveStep::Done)) else{return Ok(())};
    let state=domain.state().map_err(|error|format!("reading the admitted destination: {error:#}"))?;
    let current=state.tasks.get(task).ok_or("The historical account move has no task")?;
    if moving.agent.as_ref()!=current.agent_id.as_ref(){
        return if moving.step==AccountMoveStep::Done{Ok(())}else{Err("The historical destination no longer matches the committed node; no account will be restarted".into())};
    }
    if current.provider!=Provider::Claude || current.account_key().account!=moving.plan.to{
        return Err("The historical destination does not match the committed account; no account will be restarted".into());
    }
    let execution=state.lifecycle_of(task).map_err(|error|error.message)?.execution().cloned().ok_or("The historical destination has no current execution")?;
    let records=state.lifecycle_journal.get(task).ok_or("The historical destination has no lifecycle journal")?;
    let admitted=records.iter().rposition(|record|matches!(record,LifecycleRecord::Admitted{node,execution:Some(identity)}
        if Some(node)==moving.agent.as_ref() && identity.instance==execution.instance().as_str()));
    if admitted.is_some_and(|at|records.iter().skip(at+1).take_while(|record|!matches!(record,LifecycleRecord::Admitted{..}))
        .any(|record|matches!(record,LifecycleRecord::ResourceAvailable))){return Ok(())}
    Err("A previous release admitted another account, but its native process was never positively observed starting. Alondra will not start that account or silently return to the old one. Cancel this node and create a new node on the account you choose".into())
}

/// Takes the task's move as far as it goes: from the step recorded to its end, done or undone.
/// An error leaves the move at its step, for the next tick.
pub(crate) async fn advance(domain: &Arc<DomainRuntime>, task: &TaskId) -> Result<AccountMoveStep, String> {
    let _one = domain.moves.moving.lock().await;
    loop {
        let moving = latest(domain, task).ok_or_else(|| format!("{task} has no account move"))?;
        let step = moving.step;
        #[cfg(test)]
        if *domain.moves.crash_after.lock().unwrap() == Some(step) && step != AccountMoveStep::Admitted {
            return Err(format!("the daemon died after {step:?}"));
        }
        match step {
            AccountMoveStep::Planned | AccountMoveStep::HostStopped => {
                if let Some(why) = refused(domain, &moving) {
                    undo(domain, &moving, &why)?;
                } else if step == AccountMoveStep::Planned {
                    stop(domain, &moving).await?;
                } else {
                    admit(domain, &moving).await?;
                }
            }
            AccountMoveStep::Admitted => finish(domain, &moving).await?,
            AccountMoveStep::Done | AccountMoveStep::Undone => return Ok(step),
        }
    }
}

/// The move does not happen: the task stays on its account's limit, and its execution is taken
/// over again as after a restart, so a host runs it when the limit lets it go on.
fn undo(domain: &DomainRuntime, moving: &AccountMove, why: &str) -> Result<(), String> {
    domain.store.lock_or_exit("the domain store").record_move_undone(&moving.task, moving.number, why)
        .map_err(|error| format!("recording that the move was undone: {error:#}"))?;
    log!("{} stays on account {}: {why}", moving.task, moving.plan.from);
    crate::sdk_owner::retake(domain, moving.task.clone());
    Ok(())
}

/// Stops the host of the execution that hit the limit, and any process that still holds its
/// folder, and records that none runs its session any more.
async fn stop(domain: &Arc<DomainRuntime>, moving: &AccountMove) -> Result<(), String> {
    let (backend, _) = domain.sdk().ok_or("this daemon has no SDK backend")?;
    let folder = backend.state_root.join(&moving.plan.instance);
    let (runtime, node) = (Arc::clone(domain), moving.plan.node.clone());
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        runtime.stop_sdk_host(&node).map_err(|crate::sdk_owner::Failed { why, .. }| format!("its host did not stop: {why}"))?;
        if folder.join("host.lock").exists() {
            alondra_agent_runtime::owned_host::EndedHost::end_holders(&folder).map_err(|error| format!("its host did not stop: {error}"))?;
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("stopping the host stopped: {error}"))??;
    domain.store.lock_or_exit("the domain store").record_move_host_stopped(&moving.task, moving.number)
        .map_err(|error| format!("recording that the host stopped: {error:#}"))
}

/// Admits the execution on the other account that resumes the session, past the limit card, and
/// records the admission in the same transaction; its owner starts at once. A refusal before the
/// commit undoes the move.
async fn admit(domain: &Arc<DomainRuntime>, moving: &AccountMove) -> Result<(), String> {
    let (backend, runtime) = domain.sdk().ok_or("this daemon has no SDK backend")?;
    let backend = backend.clone();
    let plugin = crate::plugins::node_in(domain.home()).ok_or("Alondra's node plugin, which the SDK node loads, was not found")?;
    let state = domain.state().map_err(|error| format!("{error:#}"))?;
    let task = state.tasks.get(&moving.task).ok_or("the task is gone")?;
    let options = crate::sdk_owner::LaunchOptions::load(&backend.state_root.join(&moving.plan.instance)).map_err(|crate::sdk_owner::Failed { why, .. }| why)?;
    let launch = crate::sdk_owner::Launch {
        task: moving.task.clone(),
        name: state.agents.get(&moving.plan.node).map_or_else(|| task.title.clone(), |agent| agent.name.clone()),
        directory: moving.plan.cwd.clone(),
        prompt: MOVED_ON.into(),
        plugin,
        options,
        model: None,
        account: moving.plan.to.clone(),
    };
    let continuation = crate::sdk_owner::Continuation::Moved { session: moving.plan.session.clone(), card: moving.plan.card.clone(), number: moving.number };
    let admitted = tokio::task::spawn_blocking(move || crate::sdk_owner::continue_session(runtime, &backend, launch, continuation))
        .await
        .map_err(|error| crate::sdk_owner::Failed::new(format!("the admission stopped: {error}")))
        .and_then(|admitted| admitted);
    match admitted {
        Ok(driver) => {
            domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));
            domain.live.forget_closed_task(&moving.task.0);
            Ok(())
        }
        Err(crate::sdk_owner::Failed { why, .. }) if latest(domain, &moving.task).is_some_and(|now| now.step == AccountMoveStep::Admitted) => {
            Err(format!("the execution on account {} was admitted but did not start: {why}", moving.plan.to))
        }
        Err(crate::sdk_owner::Failed { why, .. }) => {
            undo(domain, moving, &format!("the execution on account {} was not admitted: {why}", moving.plan.to))?;
            Err(why)
        }
    }
}

/// Makes sure the execution on the other account runs, closes the node that hit the limit and
/// records the end of the move.
async fn finish(domain: &Arc<DomainRuntime>, moving: &AccountMove) -> Result<(), String> {
    if let Err(why)=require_started_destination(domain,&moving.task){
        crate::sdk_owner::lost(domain,&moving.task,&why);
        return Err(why);
    }
    let agent = moving.agent.clone().ok_or("the admission names no node")?;
    if !domain.owns_sdk_host(&agent) {
        let (backend, runtime) = domain.sdk().ok_or("this daemon has no SDK backend")?;
        let backend = backend.clone();
        let plugin = crate::plugins::node_in(domain.home()).ok_or("Alondra's node plugin, which the SDK node loads, was not found")?;
        let task = moving.task.clone();
        let driver = tokio::task::spawn_blocking(move || {
            crate::sdk_owner::adopt(runtime, &backend, task, Some(&plugin))
        })
        .await
        .map_err(|error| format!("starting the SDK node stopped: {error}"))?
        .map_err(|crate::sdk_owner::Failed { why, .. }| format!("the SDK node did not start: {why}"))?;
        domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));
    }
    if let Err(failure) = crate::domain_agent_lifecycle::node_event(domain, &moving.plan.node, NodeEvent::SessionClosed) {
        log!("could not close {} after its task moved to account {}: {failure}", moving.plan.node, moving.plan.to);
    }
    domain.store.lock_or_exit("the domain store").record_move_done(&moving.task, moving.number)
        .map_err(|error| format!("recording the end of the move: {error:#}"))?;
    log!("{} goes on with account {}: {agent} resumes its Claude session after account {} reached its usage limit", moving.task, moving.plan.to, moving.plan.from);
    Ok(())
}

/// Plans the move of each task its account's usage limit holds between turns, once per limit it
/// hits and not again before the limit it last left resets, to an account that can take work.
fn plan_held(domain: &DomainRuntime) {
    if !enabled(domain) {
        return;
    }
    let Ok(state) = domain.state() else { return };
    let (journal, policy, now) = (moves(domain), policy(domain), alondra_core::now_millis());
    for held in state.claude_held().into_iter().filter(|held| held.hold == ClaudeHold::Usage) {
        if held.provider != Provider::Claude || state.tasks.get(&held.task).is_none_or(|task|task.selection.is_some()){continue;}
        let tried = journal.iter().filter(|moving| moving.task == held.task)
            .any(|moving| !moving.step.ended() || moving.plan.card == held.card || (moving.step == AccountMoveStep::Done && moving.plan.until > now));
        if tried || state.held_by_limit(&held.task).is_none() {
            continue;
        }
        let Some(to) = destination(&state, policy, &held.account, now) else { continue };
        if let Err(error) = plan(domain, &held, to, now) {
            log!("{} waits on the usage limit of account {}: {error:#}", held.task, held.account);
        }
    }
}

async fn tick(domain: &Arc<DomainRuntime>) {
    plan_held(domain);
    let open: Vec<TaskId> = moves(domain).into_iter().filter(|moving| !moving.step.ended()).map(|moving| moving.task).collect();
    for task in open {
        if let Err(why) = advance(domain, &task).await {
            log!("the move of {task} to another account waits: {why}");
        }
    }
}

/// Goes on with every move a previous daemon left, then, while the daemon runs, moves each task
/// its account's limit holds when the setting is on. Runs only with an SDK backend.
pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    if domain.sdk().is_none() {
        return;
    }
    let mut changed = domain.subscribe();
    loop {
        tick(&domain).await;
        tokio::select! {
            _ = changed.changed() => {}
            () = tokio::time::sleep(EVERY) => {}
        }
    }
}

#[cfg(test)]
#[path = "account_move/tests.rs"]
mod tests;
