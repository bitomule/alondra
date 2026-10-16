//! Brings a node back when its connection is gone while its task is still open. A node does not
//! simply disconnect: its Claude Code background session is restarted with `claude respawn <id>`
//! ("Restart a background session ... so it picks up the current Claude binary", `claude respawn
//! --help`, Claude Code 2.1.287), a bounded number of times, and its mod's `session.start`
//! reconnects it. The node machine has it lost while it waits for the fleet gate, and restarting,
//! shown as "Reconnecting", only once the gate let it back. When restarts keep failing the user
//! gets a card with the error, not a node state.
//!
//! A session the user stops, kills or deletes in Claude agents is not a crash: Claude Code writes
//! the state `stopped` into its job file (`crashed` for a signal), or removes the job folder, and
//! restarting it would undo what the user chose. Such a node leaves Fleet and its task goes back
//! to not started, waiting for a relaunch.
use crate::domain_runtime::DomainRuntime;
use crate::orchestrate::Launcher;
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, NodeEvent, NodePhase, QuestionOption, TaskId, PROTOCOL_VERSION};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const RETRY: &str = "retry";
const CANCEL: &str = "cancel";
const RELAUNCH: &str = "relaunch";
const STALL_CHECK: Duration = Duration::from_secs(30);
const DISK_RECHECK: Duration = Duration::from_secs(10);
const GATE_RECHECK: Duration = Duration::from_secs(30);
const CONTINUE: &str = "Alondra restarted your session because it had stopped processing in the middle of a turn. Check where you were and continue your task.";
const CARRY_ON: &str = "Your session had ended while your task was still open, and Alondra restarted it. Check where you were and continue your task.";

pub(crate) struct Timing {
    /// How long a lost connection gets to come back by itself: the mod redials from 1 s up to 30 s.
    pub grace: Duration,
    /// How long a restarted session gets to connect.
    pub settle: Duration,
    /// The wait before each further restart.
    pub pauses: [Duration; 2],
}

impl Timing {
    pub(crate) const LIVE: Timing = Timing {
        grace: Duration::from_secs(35),
        settle: Duration::from_secs(90),
        pauses: [Duration::from_secs(60), Duration::from_secs(300)],
    };
    /// A sandbox with fast timing, so an end-to-end run sees a revival within seconds.
    pub(crate) const SANDBOX: Timing = Timing {
        grace: Duration::from_secs(3),
        settle: Duration::from_secs(20),
        pauses: [Duration::from_secs(2), Duration::from_secs(5)],
    };
}

/// The sessions that lost their connection, waiting for `run`.
pub(crate) struct Losses {
    sender: mpsc::UnboundedSender<String>,
    receiver: std::sync::Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

impl Default for Losses {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver: std::sync::Mutex::new(Some(receiver)) }
    }
}

impl Losses {
    pub(crate) fn report(&self, session: &str) {
        let _ = self.sender.send(session.to_owned());
    }

    pub(crate) fn take(&self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.receiver.lock().expect("losses poisoned").take()
    }
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, timing: &'static Timing) {
    let Some(mut lost) = domain.live.losses.take() else { return };
    if let Ok(state) = domain.state() {
        for agent in state.agents.keys() {
            domain.live.losses.report(&agent.0);
        }
    }
    tokio::spawn(watch_stalls(domain.clone()));
    while let Some(session) = lost.recv().await {
        let domain = domain.clone();
        tokio::spawn(async move { revive(&domain, &session, &crate::orchestrate::ClaudeCli, timing).await });
    }
}

async fn watch_stalls(domain: Arc<DomainRuntime>) {
    let mut reminded = std::collections::HashMap::new();
    loop {
        tokio::time::sleep(STALL_CHECK).await;
        look_at_shells(&domain);
        let now = alondra_core::now_millis();
        for session in stalled_sessions(&domain, now) {
            let domain = domain.clone();
            tokio::spawn(async move { revive_stalled(&domain, &session, &crate::orchestrate::ClaudeCli, &Timing::LIVE).await });
        }
        for agent in crate::domain_transport::background_over(&domain) {
            if let Err(error) = crate::domain_agent_lifecycle::node_event(&domain, &agent, alondra_core::domain::NodeEvent::BackgroundEnded) {
                log!("could not record that the background work of {agent} is over: {error}");
            }
        }
        for (session, text) in crate::domain_transport::behind_hung_work(&domain, now, &mut reminded) {
            if domain.live.pushes.message(&session, crate::node_push::ALONDRA, &text, false).is_err() {
                log!("could not send {session} back to its task: its connection is gone");
            }
        }
    }
}

/// Records, for every connected session, whether it has a shell call running and how much CPU it uses.
fn look_at_shells(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    let table = crate::process_watch::snapshot();
    for agent in state.agents.keys() {
        look_at_job(domain, &agent.0);
        let Some(pid) = domain.live.mods.session(&agent.0).and_then(|reported| reported.peer_pid) else { continue };
        domain.live.mods.set_shell_alive(&agent.0, crate::process_watch::shell_running(&table, pid), alondra_core::now_millis());
        if let Some(cpu) = crate::process_watch::session_cpu(&table, pid) {
            domain.live.mods.sample_cpu(&agent.0, cpu, alondra_core::now_millis());
        }
    }
}

/// Reads what Claude Code's own job state says about the session.
fn look_at_job(domain: &DomainRuntime, session: &str) {
    let Some(reported) = domain.live.mods.session(session) else { return };
    let dir = reported.job_dir.clone().or_else(|| crate::job_state::locate(&crate::watcher::claude_home(), session));
    let job = dir.as_deref().and_then(|dir| crate::job_state::read_in(dir, session));
    let dir = dir.filter(|dir| job.is_some() || reported.job_dir.as_ref() == Some(dir));
    domain.live.mods.set_job(session, job, dir);
}

/// Whether the user stopped, killed or deleted the session in Claude agents: its job file says
/// `stopped`, or the job folder Alondra last saw is gone. A session whose job was never seen is
/// not known to be closed.
pub(crate) fn session_ended(domain: &DomainRuntime, session: &str) -> bool {
    session_ended_in(domain, &crate::watcher::claude_home(), session)
}

/// `session_ended` with Claude Code's home at `claude_home`.
pub(crate) fn session_ended_in(domain: &DomainRuntime, claude_home: &std::path::Path, session: &str) -> bool {
    let known = domain.live.mods.session(session).and_then(|reported| reported.job_dir);
    let Some(dir) = known.or_else(|| crate::job_state::locate(claude_home, session)) else { return false };
    !dir.exists() || crate::job_state::read_in(&dir, session).is_some_and(|job| job.phase == crate::job_state::Phase::Stopped)
}

/// The sessions whose agent works on an open task and whose mod is connected, yet shows no
/// activity inside a turn: the session stopped processing and nothing will resume it. Only a node
/// the node machine has in a turn can stall: a quiet mod says nothing of one paused (the gate
/// brings it back), waiting on the user, resting or being brought back already. A session with a
/// held card open waits for the user: restarting it would cut the call and withdraw the card, and
/// the node would put the same card to the user again.
pub(crate) fn stalled_sessions(domain: &DomainRuntime, now: alondra_core::Millis) -> Vec<String> {
    let Ok(state) = domain.state() else { return vec![] };
    state.agents.values()
        .filter(|agent| matches!(agent.phase, NodePhase::Working | NodePhase::Delivered { in_turn: true }))
        .map(|agent| agent.id.0.clone())
        .filter(|session| open_task(&state, session) && !domain.live.is_closed(session))
        .filter(|session| !crate::domain_holds::waits_on_user(&state, session))
        .filter(|session| domain.live.gate.paused(&domain, session).is_none())
        .filter(|session| domain.live.mods.session(session).is_some_and(|reported| reported.silent(now)))
        .collect()
}

pub(crate) fn has_open_task(domain: &DomainRuntime, session: &str) -> bool {
    domain.state().is_ok_and(|state| open_task(&state, session))
}

fn open_task(state: &alondra_core::domain::DomainState, session: &str) -> bool {
    let agent = AgentId(session.to_owned());
    state.agents.contains_key(&agent) && state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && !task.archived && task.wrap_up.asked_at.is_none())
}

/// Restarts a session that holds its connection but stopped processing, and tells it to carry on
/// once the restarted session connects: its unanswered messages are written again by the new
/// connection. A restart that keeps failing ends in the same card as a lost connection.
pub(crate) async fn revive_stalled(domain: &DomainRuntime, session: &str, launcher: &impl Launcher, timing: &Timing) {
    if !domain.live.begin_watch(session) {
        return;
    }
    let short: String = session.chars().take(8).collect();
    let mut error = String::new();
    'rounds: loop {
        for attempt in 0..=timing.pauses.len() {
            if attempt > 0 {
                tokio::time::sleep(timing.pauses[attempt - 1]).await;
            }
            domain.live.usage_pause.until_clear().await;
            while domain.live.machine_watch.disk_low() {
                tokio::time::sleep(DISK_RECHECK).await;
            }
            if !until_admitted(domain, session, || still_stalled(domain, session)).await {
                break 'rounds;
            }
            let previous = domain.live.mods.session(session).and_then(|reported| reported.connection);
            match launcher.respawn(&short).await {
                Ok(()) if domain.live.mods.wait_reconnected(session, previous, timing.settle).await => {
                    tell_to_continue(domain, session).await;
                    break 'rounds;
                }
                Ok(()) => error = "the restarted session did not connect back to Alondra".into(),
                Err(reason) => error = reason,
            }
        }
        if !still_stalled(domain, session) || !ask_to_retry(domain, session, &error, launcher).await {
            break;
        }
    }
    domain.live.end_watch(session);
}

/// Waits for the fleet gate to let the session back to work, as work that resumes, so nodes come
/// back one at a time within the limit; false once `still` says it no longer has to come back.
async fn until_admitted(domain: &DomainRuntime, session: &str, still: impl Fn() -> bool) -> bool {
    use crate::fleet_gate::{Key, Waiting};
    let gate = &domain.live.gate;
    let mut turn = gate.subscribe();
    let key = Key::Resume(session.to_owned());
    let since = alondra_core::now_millis();
    loop {
        if !still() {
            gate.forget(&key);
            return false;
        }
        let priority = domain.state().ok()
            .and_then(|state| crate::fleet_gate::open_task_of(&state, session).and_then(|task| state.tasks.get(&task).map(|task| task.priority)))
            .unwrap_or_default();
        match gate.ask(domain, Waiting { key: key.clone(), priority, resuming: true, since }, alondra_core::now_millis()) {
            Ok(()) => return true,
            Err(hold) => log!("{session} waits at the fleet gate to come back: {hold:?}"),
        }
        tokio::select! {
            _ = turn.changed() => {}
            () = tokio::time::sleep(GATE_RECHECK) => {}
        }
    }
}

async fn tell_to_continue(domain: &DomainRuntime, session: &str) {
    say(domain, session, CONTINUE, false).await;
}

/// A restarted session starts no turn of its own (`claude respawn` resumes the conversation and
/// waits): the gate's resume it missed, else a word to carry on when its task still needs work
/// from it rather than an answer from the user.
async fn tell_to_carry_on(domain: &DomainRuntime, session: &str) {
    if let Some(owed) = domain.live.gate.take_owed(session) {
        say(domain, session, &owed, true).await;
    } else if needs_work(domain, session) {
        say(domain, session, CARRY_ON, false).await;
    }
}

fn needs_work(domain: &DomainRuntime, session: &str) -> bool {
    let Ok(state) = domain.state() else { return false };
    let facts = state.task_facts(&AgentId(session.to_owned()));
    facts.open && !facts.closing && !facts.card && !facts.held && !facts.delivered
}

/// The mod's connection is recorded a moment before its push link attaches.
async fn say(domain: &DomainRuntime, session: &str, text: &str, as_user: bool) {
    for _ in 0..250 {
        if domain.live.pushes.message(session, crate::node_push::ALONDRA, text, as_user).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn still_stalled(domain: &DomainRuntime, session: &str) -> bool {
    stalled_sessions(domain, alondra_core::now_millis()).iter().any(|stalled| stalled == session)
}

/// Whether the session still has to be brought back: its agent works on an open task and its mod
/// holds no connection. A paused node is the gate's: it resumes it in its turn, and brings it back
/// first if its session is gone.
fn needed(domain: &DomainRuntime, session: &str) -> bool {
    let Ok(state) = domain.state() else { return false };
    let agent = AgentId(session.to_owned());
    state.agents.get(&agent).is_some_and(|agent| !matches!(agent.phase, NodePhase::Paused(_)))
        && state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && !task.archived && task.wrap_up.asked_at.is_none())
        && !domain.live.is_closed(session)
        && !domain.live.mods.connected(session)
}

/// Only a session that still has to be brought back is watched: the watch keeps cleanup and the
/// task closer off the session, and the daemon reports every known session as lost when it starts.
pub(crate) async fn revive(domain: &DomainRuntime, session: &str, launcher: &impl Launcher, timing: &Timing) {
    if !needed(domain, session) || !domain.live.begin_watch(session) {
        return;
    }
    if !domain.live.mods.wait_connected(session, timing.grace).await && needed(domain, session) {
        restart_until_back(domain, session, launcher, timing).await;
    }
    domain.live.end_watch(session);
}

async fn restart_until_back(domain: &DomainRuntime, session: &str, launcher: &impl Launcher, timing: &Timing) {
    let short: String = session.chars().take(8).collect();
    loop {
        let mut error = String::new();
        for attempt in 0..=timing.pauses.len() {
            if attempt > 0 {
                tokio::time::sleep(timing.pauses[attempt - 1]).await;
            }
            if closed_by_user(domain, session) || !until_usage_clear(domain, session).await {
                return;
            }
            if !until_admitted(domain, session, || needed(domain, session) && !session_ended(domain, session)).await
                || closed_by_user(domain, session)
            {
                return;
            }
            move_node(domain, session, NodeEvent::RevivalAdmitted);
            match launcher.respawn(&short).await {
                Ok(()) if domain.live.mods.wait_connected(session, timing.settle).await => {
                    tell_to_carry_on(domain, session).await;
                    return;
                }
                Ok(()) => error = "the restarted session did not connect back to Alondra".into(),
                Err(reason) => error = reason,
            }
            move_node(domain, session, NodeEvent::SessionLost);
        }
        if !needed(domain, session) || !ask_to_retry(domain, session, &error, launcher).await {
            return;
        }
    }
}

/// The node machine alone says whether the node waits at the gate or is being restarted: the gate
/// letting it back makes it restart, holding a slot, and a restart that did not connect back leaves
/// it lost, waiting for the gate again.
fn move_node(domain: &DomainRuntime, session: &str, event: NodeEvent) {
    let key = event.key();
    if let Err(failure) = crate::domain_agent_lifecycle::node_event(domain, &AgentId(session.to_owned()), event) {
        log!("could not record {key} for {session}: {failure}");
    }
}

/// A session the user stopped or deleted is never waited for: it leaves the fleet and frees its
/// task at once, whatever the usage limit or the fleet gate say.
fn closed_by_user(domain: &DomainRuntime, session: &str) -> bool {
    if !session_ended(domain, session) {
        return false;
    }
    if let Err(failure) = user_closed(domain, session) {
        log!("could not record that {session} was closed by the user: {failure:#}");
    }
    true
}

/// Waits for the usage limit to clear; false once the session turns out to be closed meanwhile.
async fn until_usage_clear(domain: &DomainRuntime, session: &str) -> bool {
    loop {
        tokio::select! {
            () = domain.live.usage_pause.until_clear() => return true,
            () = tokio::time::sleep(GATE_RECHECK) => {
                if closed_by_user(domain, session) {
                    return false;
                }
            }
        }
    }
}

/// The user closed the session: the node leaves Fleet, is not brought back, and the tasks it had
/// open go back to not started with no node, to be relaunched. What a relaunch needs of the node
/// is kept.
/// A session whose replacement stopped short because it could not be stopped ends for that
/// reason, and the orchestrator hears that its tasks are free again.
pub(crate) fn user_closed(domain: &DomainRuntime, session: &str) -> anyhow::Result<()> {
    let cwd = domain.live.mods.session(session).and_then(|reported| reported.cwd);
    let Some(why) = domain.live.take_aborted_replace(session) else {
        take_out(domain, session, "node closed by the user", cwd)?;
        return Ok(());
    };
    let freed = take_out(domain, session, REPLACE_ABORTED, cwd)?;
    let Ok(state) = domain.state() else { return Ok(()) };
    for task_id in freed {
        let title = state.tasks.get(&task_id).map_or_else(|| task_id.0.clone(), |task| task.title.clone());
        let _ = domain.tell_orchestrator(&format!("replace-aborted-{task_id}-{}", crate::auth::random_hex(4)), format!(
            "“{title}” ({task_id}) has no node now: its node ended after your replacement of it stopped short, because Alondra could \
             not stop it then ({why}). No new node was started. Start one with launch (task {task_id}), without replace."));
    }
    Ok(())
}

pub(crate) const REPLACE_ABORTED: &str = "node ended after the orchestrator's replacement could not stop it";

/// `user_closed` for a session that ends for `reason`; a relaunch of its tasks starts in `cwd`.
/// The tasks it frees.
pub(crate) fn take_out(domain: &DomainRuntime, session: &str, reason: &str, cwd: Option<String>) -> anyhow::Result<Vec<TaskId>> {
    let agent = AgentId(session.to_owned());
    let state = domain.state()?;
    let name = state.agents.get(&agent).map_or_else(|| format!("node/{}", &session[..session.len().min(8)]), |known| known.name.clone());
    let node = crate::client_routes::ClosedNode { name, cwd };
    let released: Vec<_> = state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && !task.archived && task.final_presentation.is_none())
        .map(|task| task.id.clone())
        .collect();
    let mut freed = vec![];
    for task_id in released {
        let state = domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("release-{}-{}", task_id.0, crate::auth::random_hex(8)).into(),
            expected_revision: state.revision, command: Command::ReleaseTask { task_id: task_id.clone(), reason: reason.into() },
        };
        match domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            Ok(_) => {
                domain.live.remember_closed_task(&task_id.0, node.clone());
                freed.push(task_id);
            }
            Err(failure) => log!("could not free {task_id} after its node was closed: {failure:?}"),
        }
    }
    crate::domain_agent_lifecycle::node_event(domain, &agent, NodeEvent::SessionClosed).map_err(|failure| anyhow::anyhow!("{failure}"))?;
    crate::domain_transport::session_ended(domain, &agent)?;
    domain.live.mark_closed(session);
    domain.live.apply(&alondra_core::Event::NodeRemoved { session_id: session.to_owned() });
    domain.forget_ended_session(session)?;
    Ok(freed)
}

/// The card for the user once restarts keep failing; true when they ask for another round. The
/// other ways out hand the node's open tasks to a new node in the same folder, or cancel them, so
/// none is left with a node nothing brings back.
async fn ask_to_retry(domain: &DomainRuntime, session: &str, error: &str, launcher: &impl Launcher) -> bool {
    let name = domain.state().ok().and_then(|state| state.agents.get(&AgentId(session.to_owned())).map(|agent| agent.name.clone()))
        .unwrap_or_else(|| session.to_owned());
    let body = format!("{name} is not responding to Alondra while its task is open, and restarting its session with `claude respawn` failed: {error}");
    let mut options = vec![QuestionOption { id: RETRY.into(), label: "Try again".into(), description: Some("Alondra restarts the same session once more".into()) }];
    if !releasable_tasks(domain, session).is_empty() {
        options.insert(0, QuestionOption { id: RELAUNCH.into(), label: "Relaunch (Recommended)".into(),
            description: Some("A new node takes over the task in the same folder, with its files and uncommitted work".into()) });
    }
    options.push(QuestionOption { id: CANCEL.into(), label: "Cancel its task".into(), description: Some("The task closes and Alondra closes the node".into()) });
    let card = match crate::domain_holds::ask(domain, session, body, options) {
        Ok(Some(card)) => card,
        Ok(None) => return false,
        Err(failure) => {
            log!("could not tell the user that {name} could not be restarted: {failure:#}");
            return false;
        }
    };
    match crate::domain_holds::wait(domain, &card).await {
        Ok(Some(answers)) if answers[0].option_id == RETRY => true,
        Ok(Some(answers)) if answers[0].option_id == RELAUNCH => {
            relaunch_elsewhere(domain, session, &answers[0].responder, launcher).await;
            false
        }
        Ok(Some(answers)) if answers[0].option_id == CANCEL => {
            cancel_tasks_of(domain, session, &answers[0].responder);
            false
        }
        _ => false,
    }
}

/// The node's open tasks a new node can take over: those with no result in front of the user.
fn releasable_tasks(domain: &DomainRuntime, session: &str) -> Vec<TaskId> {
    let agent = AgentId(session.to_owned());
    domain.state().map(|state| state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && !task.archived && task.final_presentation.is_none())
        .map(|task| task.id.clone()).collect()).unwrap_or_default()
}

/// The user chose a new node for what this one could not be brought back for: the node closes,
/// its tasks are freed and each starts again, in their name, in the folder it worked in.
async fn relaunch_elsewhere(domain: &DomainRuntime, session: &str, user: &str, launcher: &impl Launcher) {
    let cwd = domain.live.mods.session(session).and_then(|reported| reported.cwd)
        .or_else(|| releasable_tasks(domain, session).iter().find_map(|task| domain.task_worktree(task).ok().flatten()));
    let freed = match take_out(domain, session, RELAUNCHED, cwd) {
        Ok(freed) => freed,
        Err(failure) => {
            log!("could not free the tasks of {session} to relaunch them: {failure:#}");
            return;
        }
    };
    let actor = Actor::User { user_id: user.to_owned() };
    for task in freed {
        let relaunched: std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, alondra_core::domain::DomainError>> + Send + '_>> =
            Box::pin(crate::domain_orchestrator_tools::relaunch(domain, domain.home(), &actor, &task, launcher));
        if let Err(error) = relaunched.await {
            log!("could not relaunch {task} after its node could not be restarted: {}", error.message);
        }
    }
}

const RELAUNCHED: &str = "node replaced after its session could not be restarted";

/// The user chose to cancel what the node could not be brought back for: done in their name.
fn cancel_tasks_of(domain: &DomainRuntime, session: &str, user: &str) {
    let agent = AgentId(session.to_owned());
    let Ok(state) = domain.state() else { return };
    for task in state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal()) {
        let Ok(current) = domain.state() else { return };
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: current.data_epoch.clone(),
            command_id: format!("revival-cancel-{}-{}", task.id.0, crate::auth::random_hex(8)).into(),
            expected_revision: current.revision, command: Command::CancelTask { task_id: task.id.clone() },
        };
        if let Err(error) = domain.execute(&Actor::User { user_id: user.to_owned() }, &envelope, None, || Ok(())) {
            log!("could not cancel {} after its node could not be restarted: {error:?}", task.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, InteractionId};
    use serde_json::json;
    use std::sync::Mutex;

    const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";
    const FAST: Timing = Timing { grace: Duration::from_millis(30), settle: Duration::from_millis(60), pauses: [Duration::from_millis(5), Duration::from_millis(5)] };

    struct Home(crate::test_support::Directory);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn phase(domain: &DomainRuntime) -> NodePhase {
        domain.state().unwrap().agents[&AgentId(SESSION.into())].phase.clone()
    }

    fn hello() -> crate::node_reports::Hello {
        serde_json::from_value(json!({"session_id": SESSION, "version": "2.1.287"})).unwrap()
    }

    /// A node whose mod connected once and then lost its connection.
    fn world(with_task: bool) -> (Home, Arc<DomainRuntime>) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let user = || Actor::User { user_id: "u".into() };
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: SESSION.into(), name: "alondra/probe".into(), at: 0 });
        if with_task {
            command(&domain, user(), "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
            command(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Probe".into(),
                agent_id: Some(SESSION.into()), review_prs: false, dependencies: Default::default(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
        }
        let connection = domain.connect_node(hello(), 1);
        domain.live.mods.disconnect(SESSION, connection);
        (Home(home), domain)
    }

    struct Respawner {
        domain: Arc<DomainRuntime>,
        calls: Mutex<Vec<String>>,
        launched_in: Mutex<Vec<String>>,
        works_from: usize,
    }

    impl Respawner {
        fn new(domain: &Arc<DomainRuntime>, works_from: usize) -> Self {
            Self { domain: domain.clone(), calls: Mutex::new(vec![]), launched_in: Mutex::new(vec![]), works_from }
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl Launcher for Respawner {
        async fn run(&self, cwd: &str, _: &[String]) -> Result<String, crate::orchestrate::Refusal> {
            self.launched_in.lock().unwrap().push(cwd.to_owned());
            Err((axum::http::StatusCode::BAD_GATEWAY, "the test launches nothing".into()))
        }

        fn plugin(&self) -> Option<std::path::PathBuf> {
            Some("/synthetic/plugin".into())
        }

        async fn stop(&self, _: &str) {}

        async fn respawn(&self, short: &str) -> Result<(), String> {
            let attempt = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(short.to_owned());
                calls.len()
            };
            if attempt < self.works_from {
                return Err("no such session".into());
            }
            self.domain.connect_node(hello(), 2);
            Ok(())
        }
    }

    async fn the_card(domain: &DomainRuntime) -> InteractionId {
        loop {
            if let Some(id) = domain.state().unwrap().interactions.keys().next() {
                return id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn answer(domain: &DomainRuntime, card: InteractionId, option: &str) {
        command(domain, Actor::User { user_id: "u".into() }, option, Command::AnswerQuestion {
            attachments: vec![], interaction_id: card, option_id: option.into(), inputs: Default::default(),
        });
    }

    #[tokio::test]
    async fn a_lost_connection_with_an_open_task_restarts_the_session_and_shows_reconnecting_only_meanwhile() {
        let (_home, domain) = world(true);
        crate::domain_agent_lifecycle::node_event(&domain, &AgentId(SESSION.into()), NodeEvent::SessionLost).unwrap();
        assert_eq!(phase(&domain), NodePhase::Lost);
        let claude = Respawner::new(&domain, 1);
        revive(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(*claude.calls.lock().unwrap(), ["ab1234cd"], "`claude respawn` takes the short id");
        assert!(domain.live.mods.connected(SESSION), "the restarted session's mod connected back");
        assert_eq!(phase(&domain), NodePhase::Starting { reviving: true, idle_since: None }, "the gate let it back: it restarts until its first turn");
        assert!(domain.state().unwrap().interactions.is_empty(), "a restart that works asks the user nothing");
    }

    #[tokio::test]
    async fn a_node_whose_task_is_in_cleanup_is_not_restarted() {
        use alondra_core::domain::DeliveryTarget;
        let (_home, domain) = world(true);
        let text = || vec![DeliveryTarget::Text { text: "The result".into() }];
        command(&domain, Actor::Agent { agent_id: SESSION.into() }, "present", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(), targets: text(),
            is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        assert!(needed(&domain, SESSION), "a task under review still needs its node");
        command(&domain, Actor::User { user_id: "u".into() }, "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets: text() });
        assert!(domain.state().unwrap().tasks[&"t".into()].wrap_up.asked_at.is_some());
        assert!(!needed(&domain, SESSION));
        assert!(!has_open_task(&domain, SESSION));
        let claude = Respawner::new(&domain, 1);
        revive(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(claude.calls(), 0, "the closer removes it; nothing restarts it");
    }

    const QUIET_FOR: i64 = alondra_core::states::TURN_QUIET_MS + 60_000;

    /// A node whose mod is connected and whose turn started a while ago, with nothing since.
    fn silent_turn(domain: &DomainRuntime) -> tokio::sync::mpsc::UnboundedReceiver<String> {
        let long_ago = alondra_core::now_millis() - QUIET_FOR;
        domain.connect_node(hello(), long_ago);
        let link = domain.live.pushes.attach(SESSION, 90);
        domain.live.mods.sample_cpu(SESSION, 500, long_ago);
        domain.live.mods.sample_cpu(SESSION, 500, long_ago + 1);
        domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "turn.start", "turn_id": "t"})).unwrap(), long_ago);
        link
    }

    #[tokio::test]
    async fn a_session_that_stopped_processing_inside_a_turn_is_restarted_and_told_to_continue() {
        let (_home, domain) = world(true);
        let mut link = silent_turn(&domain);
        assert_eq!(stalled_sessions(&domain, alondra_core::now_millis()), [SESSION], "connected, in a turn, silent: stalled");
        let claude = Respawner::new(&domain, 1);
        revive_stalled(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(*claude.calls.lock().unwrap(), ["ab1234cd"]);
        let line: serde_json::Value = serde_json::from_str(&link.try_recv().unwrap()).unwrap();
        assert!(line["text"].as_str().unwrap().contains("continue your task"), "{line}");
        assert!(stalled_sessions(&domain, alondra_core::now_millis()).is_empty(), "the restarted session is not stalled");
        assert!(domain.state().unwrap().interactions.is_empty());
    }

    /// After a daemon restart Alondra no longer knows the node is inside its plan call, yet the
    /// plan's card is still open in the domain: the node waits for the user and is left alone.
    #[tokio::test]
    async fn a_silent_session_whose_held_card_is_open_waits_for_the_user_and_is_never_restarted() {
        let (_home, domain) = world(true);
        let _link = silent_turn(&domain);
        let card = serde_json::from_value(json!({"session_id": SESSION, "id": "call-1", "kind": "plan", "input": {"plan": "- [ ] Ship it"}})).unwrap();
        assert!(matches!(crate::domain_holds::post(&domain, &card, true).unwrap(), crate::domain_holds::Posted::Held(_)));
        assert!(stalled_sessions(&domain, alondra_core::now_millis()).is_empty(), "waiting for the user, not stalled");
        crate::domain_holds::release_call(&domain, SESSION, "call-1").unwrap();
        assert_eq!(stalled_sessions(&domain, alondra_core::now_millis()), [SESSION], "the card is gone: the usual rule again");
    }

    #[test]
    fn a_session_that_ends_with_open_work_keeps_its_settings_file_for_respawn_and_gets_a_new_bearer_when_it_connects() {
        let agent = AgentId(SESSION.into());
        let (home, domain) = world(true);
        let path = crate::domain_sessions::prepare(&home.0, "launch-1").unwrap();
        let bearer = domain.bind_agent(SESSION).unwrap();
        crate::domain_sessions::bind(&path, &agent, bearer.expose_for_child_environment()).unwrap();
        let capability = |path: &std::path::Path| {
            let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            settings["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().map(str::to_owned)
        };
        assert!(!crate::domain_sessions::reissue(&domain, "launch-1", &agent).unwrap(), "a live session keeps its bearer");

        crate::domain_transport::session_ended(&domain, &agent).unwrap();
        assert!(path.exists(), "`claude respawn` passes this path again and must find it");
        assert_eq!(capability(&path), None, "but the revoked bearer is gone from it");

        assert!(crate::domain_sessions::reissue(&domain, "launch-1", &agent).unwrap());
        let reissued = capability(&path).expect("the restarted session has a bearer");
        assert_ne!(reissued, bearer.expose_for_child_environment());
        assert!(!crate::domain_sessions::reissue(&domain, "launch-1", &agent).unwrap(), "once");

        let (home, finished) = world(false);
        let path = crate::domain_sessions::prepare(&home.0, "launch-2").unwrap();
        let bearer = finished.bind_agent(SESSION).unwrap();
        crate::domain_sessions::bind(&path, &agent, bearer.expose_for_child_environment()).unwrap();
        crate::domain_transport::session_ended(&finished, &agent).unwrap();
        assert!(!path.exists(), "no open work: nothing will respawn it");
    }

    #[tokio::test]
    async fn a_session_with_recent_activity_or_no_open_task_is_not_stalled() {
        let (_home, domain) = world(true);
        let _link = silent_turn(&domain);
        domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "session.measure"})).unwrap(), alondra_core::now_millis());
        assert!(stalled_sessions(&domain, alondra_core::now_millis()).is_empty(), "any report is activity");
        let (_home, finished) = world(false);
        let _link = silent_turn(&finished);
        assert!(stalled_sessions(&finished, alondra_core::now_millis()).is_empty(), "no open task: nothing to resume");
    }

    fn move_node(domain: &DomainRuntime, id: &str, event: NodeEvent) {
        command(domain, Actor::System, id, Command::MoveNode { agent_id: SESSION.into(), event, at: alondra_core::now_millis() });
    }

    fn declared() -> NodeEvent {
        NodeEvent::Paused { pause: alondra_core::domain::Pause {
            reason: alondra_core::domain::PauseReason::Declared { reason: "The build queue is full".into() },
            since: alondra_core::now_millis(), cut: None, turn_since: None, cleanup: alondra_core::domain::PauseCleanup::Done, released: false,
        } }
    }

    /// 13 October: 1d6d53c5 ended its turn blocked on the user without its turn end reaching
    /// Alondra, so its mod still said "in a turn"; it was queued as stalled 73 times. Only the node
    /// machine says whether a turn runs: once it says the turn is over, a quiet mod is no stall.
    #[tokio::test]
    async fn a_quiet_mod_is_no_stall_once_the_node_machine_says_no_turn_runs() {
        let (_home, domain) = world(true);
        let _link = silent_turn(&domain);
        assert_eq!(stalled_sessions(&domain, alondra_core::now_millis()), [SESSION]);
        command(&domain, Actor::System, "listed", Command::ReconcileNodes {
            listed: [(SESSION.into(), alondra_core::domain::Listed::Blocked)].into(), at: alondra_core::now_millis(),
        });
        assert_eq!(domain.state().unwrap().node_phase(&SESSION.into()), Some(&NodePhase::WaitingUser { in_turn: false }));
        assert!(stalled_sessions(&domain, alondra_core::now_millis()).is_empty(), "blocked on the user, not stalled");
    }

    /// 13 October: db23219f, paused and without a session, logged "waits at the fleet gate to come
    /// back" 4154 times since 9 October, asking the gate under the key the gate itself resumes it by.
    #[tokio::test]
    async fn a_paused_node_whose_session_is_gone_is_left_to_the_gate() {
        let (_home, domain) = world(true);
        move_node(&domain, "turn", NodeEvent::TurnStarted);
        move_node(&domain, "paused", declared());
        assert!(!needed(&domain, SESSION));
        let claude = Respawner::new(&domain, 1);
        tokio::time::timeout(Duration::from_secs(5), revive(&domain, SESSION, &claude, &FAST)).await
            .expect("revival leaves a paused node at once");
        assert_eq!(claude.calls(), 0, "the gate brings it back in its turn");
    }

    #[tokio::test]
    async fn a_restart_that_keeps_failing_for_a_stalled_session_ends_in_a_card() {
        let (_home, domain) = world(true);
        let _link = silent_turn(&domain);
        let claude = Arc::new(Respawner::new(&domain, usize::MAX));
        let watching = tokio::spawn({
            let (domain, claude) = (domain.clone(), claude.clone());
            async move { revive_stalled(&domain, SESSION, &*claude, &FAST).await }
        });
        let card = the_card(&domain).await;
        assert_eq!(claude.calls(), 3);
        answer(&domain, card, "cancel");
        watching.await.unwrap();
    }

    #[tokio::test]
    async fn a_connection_that_comes_back_by_itself_is_not_restarted() {
        let (_home, domain) = world(true);
        let claude = Respawner::new(&domain, 1);
        let back = tokio::spawn({
            let domain = domain.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                domain.connect_node(hello(), 3);
            }
        });
        let patient = Timing { grace: Duration::from_secs(10), ..FAST };
        revive(&domain, SESSION, &claude, &patient).await;
        back.await.unwrap();
        assert_eq!(claude.calls(), 0);
    }

    #[tokio::test]
    async fn without_an_open_task_or_after_the_user_closed_it_nothing_is_restarted() {
        let (_home, finished) = world(false);
        let claude = Respawner::new(&finished, 1);
        revive(&finished, SESSION, &claude, &FAST).await;
        assert_eq!(claude.calls(), 0, "no open task: the node leaves");

        let (_home, closed) = world(true);
        closed.live.mark_closed(SESSION);
        let claude = Respawner::new(&closed, 1);
        revive(&closed, SESSION, &claude, &FAST).await;
        assert_eq!(claude.calls(), 0, "a node the user closed stays closed");
    }

    #[tokio::test]
    async fn restarts_are_bounded_and_then_the_user_gets_a_card_with_the_error_not_a_node_state() {
        let (_home, domain) = world(true);
        let claude = Arc::new(Respawner::new(&domain, usize::MAX));
        let watching = tokio::spawn({
            let (domain, claude) = (domain.clone(), claude.clone());
            async move { revive(&domain, SESSION, &*claude, &FAST).await }
        });
        let card = the_card(&domain).await;
        assert_eq!(claude.calls(), 3, "one restart and two retries, no more");
        assert_eq!(phase(&domain), NodePhase::Lost, "no restart runs while the card waits: it holds no slot");
        let state = domain.state().unwrap();
        assert!(state.interactions[&card].body.contains("no such session"), "the error reaches the user: {}", state.interactions[&card].body);
        assert!(state.interactions[&card].body.contains("alondra/probe"));
        answer(&domain, card, "cancel");
        watching.await.unwrap();
        assert_eq!(claude.calls(), 3, "cancelling stops");
        let state = domain.state().unwrap();
        assert!(state.tasks.values().all(|task| task.state.terminal()), "no task is left with a node nothing brings back");
    }

    /// The same session cannot be brought back: the card's first way out hands the task to a new
    /// node in the folder the old one worked in, where its uncommitted work still is.
    #[tokio::test]
    async fn a_session_that_cannot_be_restarted_is_relaunched_in_its_folder_from_the_card() {
        let (home, domain) = world(true);
        let folder = home.0.join("worktrees/t-probe");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("staged.rs"), "uncommitted work").unwrap();
        let hello = json!({"session_id": SESSION, "version": "2.1.291", "cwd": folder.to_str().unwrap()});
        let connection = domain.connect_node(serde_json::from_value(hello).unwrap(), 3);
        domain.live.mods.disconnect(SESSION, connection);
        let claude = Arc::new(Respawner::new(&domain, usize::MAX));
        let watching = tokio::spawn({
            let (domain, claude) = (domain.clone(), claude.clone());
            async move { revive(&domain, SESSION, &*claude, &FAST).await }
        });
        let card = the_card(&domain).await;
        let state = domain.state().unwrap();
        let options: Vec<&str> = match &state.interactions[&card].payload {
            alondra_core::domain::InteractionPayload::Question { options, .. } => options.iter().map(|option| option.label.as_str()).collect(),
            other => panic!("a question, not {other:?}"),
        };
        assert_eq!(options, ["Relaunch", "Try again", "Cancel its task"]);
        assert_eq!(state.interactions[&card].card.recommended.as_deref(), Some(RELAUNCH));
        answer(&domain, card, RELAUNCH);
        watching.await.unwrap();
        assert_eq!(claude.calls(), 3, "no more restarts of the old session");
        assert_eq!(*claude.launched_in.lock().unwrap(), [folder.to_str().unwrap()], "a new node starts in the old one's folder");
        let state = domain.state().unwrap();
        assert_eq!(state.agents[&AgentId(SESSION.into())].phase, alondra_core::domain::NodePhase::Closed);
        assert_eq!(std::fs::read_to_string(folder.join("staged.rs")).unwrap(), "uncommitted work");
    }

    #[tokio::test]
    async fn asking_to_try_again_runs_another_round_of_restarts() {
        let (_home, domain) = world(true);
        let claude = Arc::new(Respawner::new(&domain, 4));
        let watching = tokio::spawn({
            let (domain, claude) = (domain.clone(), claude.clone());
            async move { revive(&domain, SESSION, &*claude, &FAST).await }
        });
        let card = the_card(&domain).await;
        assert_eq!(claude.calls(), 3);
        answer(&domain, card, "retry");
        watching.await.unwrap();
        assert_eq!(claude.calls(), 4);
        assert!(domain.live.mods.connected(SESSION), "the next round worked");
    }

    #[tokio::test]
    async fn no_session_is_restarted_while_usage_is_exhausted_and_it_is_when_usage_is_back() {
        let (_home, domain) = world(true);
        let claude = Arc::new(Respawner::new(&domain, 1));
        domain.live.usage_pause.exhaust(Some(alondra_core::now_millis() + 3_600_000), alondra_core::now_millis());
        let watching = tokio::spawn({
            let (domain, claude) = (domain.clone(), claude.clone());
            async move { revive(&domain, SESSION, &*claude, &FAST).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(claude.calls(), 0, "a restarted session would only hit the limit again");
        assert!(!watching.is_finished());
        domain.live.usage_pause.resume(alondra_core::now_millis());
        watching.await.unwrap();
        assert_eq!(claude.calls(), 1, "back: the session is restarted once");
        assert!(domain.live.mods.connected(SESSION));
    }

    /// The job folder Claude Code keeps for the session, as Alondra last saw it.
    fn job_folder(home: &Home, domain: &DomainRuntime, state: &str) -> std::path::PathBuf {
        let dir = home.0.join("jobs/ab1234cd");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), format!(
            r#"{{"state":"{state}","detail":"d","sessionId":"{SESSION}","resumeSessionId":"{SESSION}","updatedAt":"2026-10-06T08:01:28.241Z"}}"#)).unwrap();
        domain.live.mods.set_job(SESSION, None, Some(dir.clone()));
        dir
    }

    fn working(domain: &DomainRuntime) {
        command(domain, Actor::Agent { agent_id: SESSION.into() }, "start", Command::StartTask { task_id: "t".into() });
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].state, alondra_core::domain::TaskState::Running);
    }

    fn freed(domain: &DomainRuntime) {
        let state = domain.state().unwrap();
        let task = &state.tasks[&"t".into()];
        assert_eq!((task.state, task.agent_id.clone(), task.started), (alondra_core::domain::TaskState::NotStarted, None, false), "the task waits for a new node");
        assert!(domain.live.is_closed(SESSION) && domain.live.closed_sessions() == [SESSION], "the node leaves Fleet");
        let closed = domain.live.closed_task("t").expect("the relaunch knows the node that was closed");
        assert_eq!(closed.name, "alondra/probe");
        assert!(state.interactions.is_empty(), "no card asks whether to retry");
    }

    #[tokio::test]
    async fn a_node_the_user_stopped_in_claude_agents_is_not_restarted_and_its_task_is_freed() {
        let (home, domain) = world(true);
        working(&domain);
        job_folder(&home, &domain, "stopped");
        let claude = Respawner::new(&domain, 1);
        revive(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(claude.calls(), 0, "no `claude respawn`");
        freed(&domain);
        assert!(!domain.live.mods.connected(SESSION));
    }

    #[tokio::test]
    async fn a_node_the_user_stopped_while_usage_is_exhausted_frees_its_task_without_waiting_for_usage() {
        let (home, domain) = world(true);
        working(&domain);
        job_folder(&home, &domain, "stopped");
        domain.live.usage_pause.exhaust(Some(alondra_core::now_millis() + 3_600_000), alondra_core::now_millis());
        let claude = Respawner::new(&domain, 1);
        tokio::time::timeout(Duration::from_secs(2), revive(&domain, SESSION, &claude, &FAST)).await
            .expect("a stopped session does not wait for the usage limit to clear");
        assert_eq!(claude.calls(), 0);
        freed(&domain);
    }

    #[tokio::test]
    async fn a_node_the_user_deleted_from_claude_agents_is_not_restarted_and_its_task_is_freed() {
        let (home, domain) = world(true);
        working(&domain);
        let dir = job_folder(&home, &domain, "idle");
        std::fs::remove_dir_all(dir).unwrap();
        let claude = Respawner::new(&domain, 1);
        revive(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(claude.calls(), 0);
        freed(&domain);
    }

    #[tokio::test]
    async fn a_node_that_crashed_is_still_restarted_and_keeps_its_task() {
        let (home, domain) = world(true);
        working(&domain);
        job_folder(&home, &domain, "crashed");
        let claude = Respawner::new(&domain, 1);
        revive(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(claude.calls(), 1, "a signal is not the user's choice");
        let state = domain.state().unwrap();
        assert_eq!(state.tasks[&"t".into()].agent_id, Some(SESSION.into()));
        assert!(!domain.live.is_closed(SESSION) && domain.live.closed_task("t").is_none());
        assert!(domain.live.mods.connected(SESSION));
    }

    #[tokio::test]
    async fn a_stopped_node_with_no_open_task_just_drops_out_as_before() {
        let (home, domain) = world(false);
        job_folder(&home, &domain, "stopped");
        let claude = Respawner::new(&domain, 1);
        revive(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(claude.calls(), 0);
        assert!(!domain.live.is_closed(SESSION) && domain.live.closed_task_ids().is_empty(), "nothing is marked or kept for it");
    }
}
