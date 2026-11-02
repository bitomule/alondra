//! Brings back a `claude --bg` node of an earlier release whose connection is gone, or that
//! stopped processing in the middle of a turn, while its task is still open: it moves to the Agent
//! SDK with its same session once the admission gate lets it back. The node machine has it lost while
//! it waits for the gate.
//!
//! A session the user stops, kills or deletes in Claude agents is not a crash: Claude Code writes
//! the state `stopped` into its job file (`crashed` for a signal), or removes the job folder, and
//! restarting it would undo what the user chose. Such a node closes and its task goes back
//! to not started, waiting for a relaunch.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, DomainState, NodeEvent, NodePhase, TaskId, TaskState, PROTOCOL_VERSION};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const STALL_CHECK: Duration = Duration::from_secs(30);
const DISK_RECHECK: Duration = Duration::from_secs(10);
const GATE_RECHECK: Duration = Duration::from_secs(30);
const CONTINUE: &str = "Alondra restarted your session because it had stopped processing in the middle of a turn. Check where you were and continue your task.";
const CARRY_ON: &str = "Your session had ended while your task was still open, and Alondra restarted it. Check where you were and continue your task.";

pub(crate) struct Timing {
    /// How long a lost connection gets to come back by itself: the mod redials from 1 s up to 30 s.
    pub grace: Duration,
}

impl Timing {
    pub(crate) const LIVE: Timing = Timing { grace: Duration::from_secs(35) };
    /// A sandbox with fast timing, so an end-to-end run sees a revival within seconds.
    pub(crate) const SANDBOX: Timing = Timing { grace: Duration::from_secs(3) };
}

/// The sessions that lost their connection, waiting for `run`.
pub(crate) struct Losses {
    sender: mpsc::UnboundedSender<String>,
    /// A holder that panics leaves it whole, so it is recovered: one take.
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
        self.receiver.lock_or_recover("the lost sessions").take()
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
    tokio::spawn(wake_when_waits_end(domain.clone()));
    while let Some(session) = lost.recv().await {
        let domain = domain.clone();
        tokio::spawn(async move { revive(&domain, &session, timing).await });
    }
}

/// A node whose task waits for other tasks is not brought back while it waits. Once a commit ends
/// the wait, its session is reported lost: if it is gone, the gate brings it back in its turn.
async fn wake_when_waits_end(domain: Arc<DomainRuntime>) {
    let mut changed = domain.subscribe();
    let mut waiting = waiting_nodes(&domain);
    while changed.changed().await.is_ok() {
        let now = waiting_nodes(&domain);
        for session in waiting.difference(&now) {
            domain.live.losses.report(session);
        }
        waiting = now;
    }
}

fn waiting_nodes(domain: &DomainRuntime) -> std::collections::BTreeSet<String> {
    let Ok(state) = domain.state() else { return Default::default() };
    state.tasks.values()
        .filter(|task| task.state == TaskState::WaitingDependency && !task.archived)
        .filter_map(|task| task.agent_id.as_ref().map(|agent| agent.0.clone()))
        .collect()
}

/// A node whose task waits for other tasks has nothing to do until they are done: whatever its
/// session does meanwhile, it is neither restarted nor told to carry on.
fn awaits_other_tasks(state: &DomainState, session: &str) -> bool {
    state.task_facts(&AgentId(session.to_owned())).awaits
}

async fn watch_stalls(domain: Arc<DomainRuntime>) {
    let mut reminded = std::collections::HashMap::new();
    loop {
        tokio::time::sleep(STALL_CHECK).await;
        look_at_shells(&domain);
        for session in stalled_sessions(&domain) {
            let domain = domain.clone();
            tokio::spawn(async move { revive_stalled(&domain, &session).await });
        }
        for agent in crate::domain_transport::background_over(&domain) {
            if let Err(error) = crate::domain_agent_lifecycle::node_event(&domain, &agent, alondra_core::domain::NodeEvent::BackgroundEnded) {
                log!("could not record that the background work of {agent} is over: {error}");
            }
        }
        for (session, text) in crate::domain_transport::behind_hung_work(&domain, &mut reminded) {
            if crate::sdk_handover::tell(&domain, &session, crate::node_push::ALONDRA, &text, false).is_err() {
                log!("could not send {session} back to its task: its connection is gone");
            }
        }
    }
}

/// Records, for every connected session, whether it has a shell call running and whether its process still runs.
fn look_at_shells(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    let table = crate::process_watch::snapshot();
    for agent in state.agents.keys() {
        look_at_job(domain, &agent.0);
        let Some(pid) = domain.live.mods.session(&agent.0).and_then(|reported| reported.peer_pid) else { continue };
        domain.live.mods.set_shell_alive(&agent.0, crate::process_watch::shell_running(&table, pid), alondra_core::now_millis());
        domain.live.mods.set_session_alive(&agent.0, crate::process_watch::session_running(&table, pid));
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

/// The sessions whose agent works on an open task and whose turn a fact stopped (`Halt`): the
/// shell of the call it waits on is gone, or its process is. Nothing will resume them. A closed
/// connection is `revive`'s, and how long a turn has been quiet stops nothing. Only a node
/// the node machine has in a turn can stall: a quiet mod says nothing of one paused (the gate
/// brings it back), waiting on the user, resting or being brought back already. A session with a
/// held card open waits for the user: restarting it would cut the call and withdraw the card, and
/// the node would put the same card to the user again.
pub(crate) fn stalled_sessions(domain: &DomainRuntime) -> Vec<String> {
    let Ok(state) = domain.state() else { return vec![] };
    state.agents.values()
        .filter(|agent| matches!(agent.phase, NodePhase::Working | NodePhase::Delivered { in_turn: true }))
        .map(|agent| agent.id.0.clone())
        .filter(|session| open_task(&state, session) && !domain.live.is_closed(session))
        .filter(|session| !awaits_other_tasks(&state, session))
        .filter(|session| !crate::sdk_handover::moved(domain, &AgentId(session.clone())))
        .filter(|session| !crate::domain_holds::waits_on_user(&state, session))
        .filter(|session| domain.live.gate.paused(&domain, session).is_none())
        .filter(|session| domain.live.mods.session(session).is_some_and(|reported| reported.halt().is_some()))
        .collect()
}

pub(crate) fn has_open_task(domain: &DomainRuntime, session: &str) -> bool {
    domain.state().is_ok_and(|state| open_task(&state, session))
}

fn open_task(state: &alondra_core::domain::DomainState, session: &str) -> bool {
    let agent = AgentId(session.to_owned());
    state.agents.contains_key(&agent) && state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent) && !task.over() && !task.archived && task.wrap_up.asked_at.is_none())
}

/// Brings back a session that holds its connection but stopped processing: it moves to the Agent
/// SDK and is told to carry on.
pub(crate) async fn revive_stalled(domain: &DomainRuntime, session: &str) {
    if !domain.live.begin_watch(session) {
        return;
    }
    domain.live.usage_pause.until_clear().await;
    while domain.live.machine_watch.disk_low() {
        tokio::time::sleep(DISK_RECHECK).await;
    }
    if until_admitted(domain, session, || still_stalled(domain, session)).await {
        bring_back_through_sdk(domain, session, Some(CONTINUE.to_owned()));
    }
    domain.live.end_watch(session);
}

/// Waits for the admission gate to let the session back to work, as work that resumes, so nodes come
/// back one at a time within the limit; false once `still` says it no longer has to come back.
async fn until_admitted(domain: &DomainRuntime, session: &str, still: impl Fn() -> bool) -> bool {
    use crate::admission::{Key, Waiting};
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
            .and_then(|state| crate::admission::open_task_of(&state, session).and_then(|task| state.tasks.get(&task).map(|task| task.priority)))
            .unwrap_or_default();
        match gate.ask(domain, Waiting { key: key.clone(), priority, resuming: true, since }, alondra_core::now_millis()) {
            Ok(()) => return true,
            Err(hold) => log!("{session} waits at the admission gate to come back: {hold:?}"),
        }
        tokio::select! {
            _ = turn.changed() => {}
            () = tokio::time::sleep(GATE_RECHECK) => {}
        }
    }
}

/// A node Alondra brings back moves to the SDK: `text` opens its same session there. With nothing to tell it, its session rests until its next
/// input, which moves it then.
fn bring_back_through_sdk(domain: &DomainRuntime, session: &str, text: Option<String>) {
    let agent = AgentId(session.to_owned());
    let Some(text) = text else {
        move_node(domain, session, NodeEvent::Listed { listed: alondra_core::domain::Listed::Done });
        return;
    };
    let Some(task) = domain.state().ok().and_then(|state| crate::admission::open_task_of(&state, session)) else { return };
    let stamped = crate::node_push::stamped(crate::node_push::ALONDRA, alondra_core::now_millis(), &text);
    if let Err(error) = crate::sdk_handover::plan(domain, &agent, &task, stamped, None, true) {
        log!("{session} was not brought back through the Agent SDK: {error:#}");
    }
}

/// What a node that comes back is told: the gate's resume it missed, else a word to carry on when
/// its task still needs work from it rather than an answer from the user.
fn carry_on_text(domain: &DomainRuntime, session: &str) -> Option<String> {
    domain.live.gate.take_owed(session).or_else(|| needs_work(domain, session).then(|| CARRY_ON.to_owned()))
}

fn needs_work(domain: &DomainRuntime, session: &str) -> bool {
    let Ok(state) = domain.state() else { return false };
    let facts = state.task_facts(&AgentId(session.to_owned()));
    facts.open && !facts.closing && !facts.card && !facts.held && !facts.delivered && !facts.awaits
}

fn still_stalled(domain: &DomainRuntime, session: &str) -> bool {
    stalled_sessions(domain).iter().any(|stalled| stalled == session)
}

/// Whether the session still has to be brought back: its agent works on an open task and its mod
/// holds no connection. A paused node is the gate's: it resumes it in its turn, and brings it back
/// first if its session is gone.
/// A node of the Agent SDK is no background session: its owner takes over its host, or replaces
/// one that ended, so it is never respawned here.
pub(crate) fn needed(domain: &DomainRuntime, session: &str) -> bool {
    let Ok(state) = domain.state() else { return false };
    let agent = AgentId(session.to_owned());
    !crate::sdk_owner::runs_through_sdk(&state, &agent)
        && !crate::sdk_handover::moved(domain, &agent)
        && state.agents.get(&agent).is_some_and(|agent| !matches!(agent.phase, NodePhase::Paused(_)))
        && state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent) && !task.over() && !task.archived && task.wrap_up.asked_at.is_none())
        && !awaits_other_tasks(&state, session)
        && !domain.live.is_closed(session)
        && !domain.live.mods.connected(session)
}

/// Only a session that still has to be brought back is watched: the watch keeps cleanup and the
/// task closer off the session, and the daemon reports every known session as lost when it starts.
pub(crate) async fn revive(domain: &DomainRuntime, session: &str, timing: &Timing) {
    if !needed(domain, session) || !domain.live.begin_watch(session) {
        return;
    }
    if !domain.live.mods.wait_connected(session, timing.grace).await && needed(domain, session) {
        bring_back(domain, session).await;
    }
    domain.live.end_watch(session);
}

/// Once the usage limit clears the node moves to the SDK; with something to be told it waits for
/// the admission gate first, as work that resumes. A session the user stopped is not brought back.
async fn bring_back(domain: &DomainRuntime, session: &str) {
    if closed_by_user(domain, session) || !until_usage_clear(domain, session).await {
        return;
    }
    let text = carry_on_text(domain, session);
    if text.is_some() && (!until_admitted(domain, session, || needed(domain, session) && !session_ended(domain, session)).await
        || closed_by_user(domain, session))
    {
        return;
    }
    bring_back_through_sdk(domain, session, text);
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

/// A session the user stopped or deleted is never waited for: it closes and frees its
/// task at once, whatever the usage limit or the admission gate say.
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
        if !domain.live.usage_pause.is_active() {
            if domain.state().is_ok_and(|state| crate::usage_pause::legacy_session_retry_ready(&state, session, alondra_core::now_millis())) {
                return true;
            }
            tokio::time::sleep(GATE_RECHECK).await;
            if closed_by_user(domain, session) { return false; }
            continue;
        }
        tokio::select! {
            () = domain.live.usage_pause.until_clear() => {},
            () = tokio::time::sleep(GATE_RECHECK) => {
                if closed_by_user(domain, session) {
                    return false;
                }
            }
        }
    }
}

/// The user closed the session: the node closes, is not brought back, and the tasks it had
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
        let _ = domain.tell_orchestrator(&format!("replace-aborted-{task_id}-{}", crate::auth::random_hex(4)), None, format!(
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
        .filter(|task| task.agent_id.as_ref() == Some(&agent) && !task.over() && !task.archived && task.final_presentation.is_none())
        .map(|task| task.id.clone())
        .collect();
    let mut freed = vec![];
    for task_id in released {
        let state = domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("release-{}-{}", task_id.0, crate::auth::random_hex(8)).into(),
            expected_revision: state.revision, command: Command::ReleaseTask { task_id: task_id.clone(), reason: reason.into() },
        };
        match domain.execute_as_system(envelope) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope};
    use serde_json::json;

    const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";
    const FAST: Timing = Timing { grace: Duration::from_millis(30) };

    /// The daemon's home and the folder of its SDK hosts.
    struct Home(crate::test_support::Directory, crate::sdk_owner::tests::HostRoot);

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn hello() -> crate::node_reports::Hello {
        serde_json::from_value(json!({"session_id": SESSION, "version": "2.1.287", "cwd": "/tmp"})).unwrap()
    }

    /// A node whose mod connected once and then lost its connection, on a daemon with the Agent
    /// SDK whose moves stop once planned, so what bringing a node back asks for is seen in its plan.
    fn world(with_task: bool) -> (Home, Arc<DomainRuntime>) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let state = crate::sdk_owner::tests::HostRoot::new();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        domain.enable_sdk(crate::sdk_owner::tests::fixture_backend(&home, &state).0);
        *domain.handovers.crash_after.lock().unwrap() = Some(crate::domain_store::HandoverStep::Planned);
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
        (Home(home, state), domain)
    }

    /// Each move to the SDK planned for the node: the input that opens its session there, and
    /// whether it was brought back rather than found at rest.
    fn moves(domain: &DomainRuntime) -> Vec<(String, bool)> {
        domain.store.lock().unwrap().handovers().unwrap().into_iter()
            .filter(|handover| handover.node.0 == SESSION)
            .map(|handover| (handover.plan.input, handover.plan.returning)).collect()
    }

    fn brought_back_told(domain: &DomainRuntime, text: &str) {
        let moves = moves(domain);
        assert_eq!(moves.len(), 1, "one move to the SDK: {moves:?}");
        assert!(moves[0].1, "as a node brought back: {moves:?}");
        assert!(moves[0].0.contains(text), "its session opens told {text:?}: {}", moves[0].0);
    }

    #[tokio::test]
    async fn a_global_capacity_return_does_not_release_a_different_accounts_revival_wait() {
        use alondra_core::accounts::{AccountId, AccountState, AccountStatus, Provider};
        use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
        let (_home, domain) = world(true);
        let status = |provider| AccountStatus { provider, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
            native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0 };
        command(&domain, Actor::System, "mixed-accounts", Command::RecordAccounts {
            accounts: vec![status(Provider::Claude), status(Provider::Codex)], reserve: alondra_core::usage::default_reserve(),
        });
        let reset = 9_007_199_254_740_991;
        command(&domain, Actor::System, "own-account-full", Command::RecordAccountUsage {
            provider: Provider::Claude, account: AccountId::default_account(), reading: UsageReading { windows: vec![WindowReading {
                kind: WindowKind::Session, used: Share::FULL, resets_at: Some(reset), exhausted: true,
            }], plan: None, complete: true }, at: 1_000, day: "1970-01-01".into(),
        });
        assert_eq!(domain.state().unwrap().account_usage.iter().find(|usage| usage.provider == Provider::Claude && usage.account == AccountId::default_account())
            .unwrap().plan_exhausted_until(2_000), Some(reset), "the fixture commits its own account's limit before polling recovery");
        domain.live.usage_pause.resume(2_000);
        let mut wait = std::pin::pin!(until_usage_clear(&domain, SESSION));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(std::future::Future::poll(wait.as_mut(), &mut context).is_pending(), "the same account still holds this disconnected node");
        assert!(moves(&domain).is_empty());
    }

    #[tokio::test]
    async fn a_lost_connection_with_an_open_task_moves_the_session_to_the_sdk_told_to_carry_on() {
        let (_home, domain) = world(true);
        crate::domain_agent_lifecycle::node_event(&domain, &AgentId(SESSION.into()), NodeEvent::SessionLost).unwrap();
        assert_eq!(domain.state().unwrap().agents[&AgentId(SESSION.into())].phase, NodePhase::Lost);
        revive(&domain, SESSION, &FAST).await;
        brought_back_told(&domain, CARRY_ON);
        assert!(!needed(&domain, SESSION), "a node moving to the SDK is not brought back again");
        assert!(domain.state().unwrap().interactions.is_empty(), "bringing it back asks the user nothing");
    }

    #[tokio::test]
    async fn a_node_whose_task_is_in_cleanup_is_not_brought_back() {
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
        revive(&domain, SESSION, &FAST).await;
        assert!(moves(&domain).is_empty(), "the closer removes it; nothing brings it back");
    }

    /// A node whose mod is connected, inside a turn whose Bash call has no shell left: two looks
    /// after the call started found none.
    fn halted_turn(domain: &DomainRuntime) -> tokio::sync::mpsc::UnboundedReceiver<String> {
        let started = alondra_core::now_millis() - 60 * 60_000;
        domain.connect_node(hello(), started);
        let link = domain.live.pushes.attach(SESSION, 90);
        domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "turn.start", "turn_id": "t"})).unwrap(), started);
        domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "tool.start", "tool": "Bash", "tool_use_id": "u"})).unwrap(), started);
        domain.live.mods.set_shell_alive(SESSION, Some(false), started + 1);
        domain.live.mods.set_shell_alive(SESSION, Some(false), started + 2);
        link
    }

    #[tokio::test]
    async fn a_session_whose_call_lost_its_shell_moves_to_the_sdk_told_to_continue() {
        let (_home, domain) = world(true);
        let _link = halted_turn(&domain);
        assert_eq!(stalled_sessions(&domain), [SESSION], "connected, in a turn, its call's shell gone: stalled");
        revive_stalled(&domain, SESSION).await;
        brought_back_told(&domain, CONTINUE);
        assert!(stalled_sessions(&domain).is_empty(), "the session moving to the SDK is not stalled");
        assert!(domain.state().unwrap().interactions.is_empty());
    }

    /// After a daemon restart Alondra no longer knows the node is inside its plan call, yet the
    /// plan's card is still open in the domain: the node waits for the user and is left alone.
    #[tokio::test]
    async fn a_stalled_session_whose_held_card_is_open_waits_for_the_user_and_is_never_restarted() {
        let (_home, domain) = world(true);
        let _link = halted_turn(&domain);
        let card = serde_json::from_value(json!({"session_id": SESSION, "id": "call-1", "kind": "plan", "input": {"plan": "- [ ] Ship it"}})).unwrap();
        assert!(matches!(crate::domain_holds::post(&domain, &card, true).unwrap(), crate::domain_holds::Posted::Held(_)));
        assert!(stalled_sessions(&domain).is_empty(), "waiting for the user, not stalled");
        crate::domain_holds::release_call(&domain, SESSION, "call-1").unwrap();
        assert_eq!(stalled_sessions(&domain), [SESSION], "the card is gone: the usual rule again");
    }

    /// The settings file an earlier release launched the node's `claude --bg` session with.
    fn earlier_launch(home: &std::path::Path, key: &str) -> std::path::PathBuf {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let dir = home.join("runtime/sessions");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).unwrap();
        let path = dir.join(format!("{key}.json"));
        let settings = json!({"env": {"ALONDRA_DOMAIN_PROTOCOL": "2", "ALONDRA_DOMAIN_SETTINGS": path.display().to_string()}});
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
        std::io::Write::write_all(&mut file, &serde_json::to_vec(&settings).unwrap()).unwrap();
        path
    }

    #[test]
    fn a_session_that_ends_with_open_work_keeps_its_settings_file_and_gets_a_new_bearer_when_it_connects() {
        let agent = AgentId(SESSION.into());
        let (home, domain) = world(true);
        let path = earlier_launch(&home.0, "launch-1");
        let bearer = domain.bind_agent(SESSION).unwrap();
        crate::domain_sessions::bind(&path, &agent, bearer.expose_for_child_environment()).unwrap();
        let capability = |path: &std::path::Path| {
            let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            settings["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().map(str::to_owned)
        };
        assert!(!crate::domain_sessions::reissue(&domain, "launch-1", &agent).unwrap(), "a live session keeps its bearer");

        crate::domain_transport::session_ended(&domain, &agent).unwrap();
        assert!(path.exists(), "the session finds this path again when it is resumed");
        assert_eq!(capability(&path), None, "but the revoked bearer is gone from it");

        assert!(crate::domain_sessions::reissue(&domain, "launch-1", &agent).unwrap());
        let reissued = capability(&path).expect("the restarted session has a bearer");
        assert_ne!(reissued, bearer.expose_for_child_environment());
        assert!(!crate::domain_sessions::reissue(&domain, "launch-1", &agent).unwrap(), "once");

        let (home, finished) = world(false);
        let path = earlier_launch(&home.0, "launch-2");
        let bearer = finished.bind_agent(SESSION).unwrap();
        crate::domain_sessions::bind(&path, &agent, bearer.expose_for_child_environment()).unwrap();
        crate::domain_transport::session_ended(&finished, &agent).unwrap();
        assert!(!path.exists(), "no open work: nothing will resume it");
    }

    #[tokio::test]
    async fn a_session_whose_call_ended_or_with_no_open_task_is_not_stalled() {
        let (_home, domain) = world(true);
        let _link = halted_turn(&domain);
        domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "model", "model": "claude-opus-5-5"})).unwrap(), alondra_core::now_millis());
        assert_eq!(stalled_sessions(&domain), [SESSION], "a report says nothing of the call's shell");
        domain.report(serde_json::from_value(json!({"session_id": SESSION, "event": "tool.end", "tool_use_id": "u"})).unwrap(), alondra_core::now_millis());
        assert!(stalled_sessions(&domain).is_empty(), "its call ended");
        let (_home, finished) = world(false);
        let _link = halted_turn(&finished);
        assert!(stalled_sessions(&finished).is_empty(), "no open task: nothing to resume");
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
    /// machine says whether a turn runs: once it says the turn is over, nothing the mod left is a stall.
    #[tokio::test]
    async fn a_stale_mod_is_no_stall_once_the_node_machine_says_no_turn_runs() {
        let (_home, domain) = world(true);
        let _link = halted_turn(&domain);
        assert_eq!(stalled_sessions(&domain), [SESSION]);
        command(&domain, Actor::System, "listed", Command::ReconcileNodes {
            listed: [(SESSION.into(), alondra_core::domain::Listed::Blocked)].into(), at: alondra_core::now_millis(),
        });
        assert_eq!(domain.state().unwrap().node_phase(&SESSION.into()), Some(&NodePhase::WaitingUser { in_turn: false }));
        assert!(stalled_sessions(&domain).is_empty(), "blocked on the user, not stalled");
    }

    /// 13 October: db23219f, paused and without a session, logged "waits at the admission gate to come
    /// back" 4154 times since 9 October, asking the gate under the key the gate itself resumes it by.
    #[tokio::test]
    async fn a_paused_node_whose_session_is_gone_is_left_to_the_gate() {
        let (_home, domain) = world(true);
        move_node(&domain, "turn", NodeEvent::TurnStarted);
        move_node(&domain, "paused", declared());
        assert!(!needed(&domain, SESSION));
        tokio::time::timeout(Duration::from_secs(5), revive(&domain, SESSION, &FAST)).await
            .expect("revival leaves a paused node at once");
        assert!(moves(&domain).is_empty(), "the gate brings it back in its turn");
    }

    #[tokio::test]
    async fn a_connection_that_comes_back_by_itself_is_not_brought_back() {
        let (_home, domain) = world(true);
        let back = tokio::spawn({
            let domain = domain.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                domain.connect_node(hello(), 3);
            }
        });
        let patient = Timing { grace: Duration::from_secs(10) };
        revive(&domain, SESSION, &patient).await;
        back.await.unwrap();
        assert!(moves(&domain).is_empty());
    }

    #[tokio::test]
    async fn without_an_open_task_or_after_the_user_closed_it_nothing_is_brought_back() {
        let (_home, finished) = world(false);
        revive(&finished, SESSION, &FAST).await;
        assert!(moves(&finished).is_empty(), "no open task: the node leaves");

        let (_home, closed) = world(true);
        closed.live.mark_closed(SESSION);
        revive(&closed, SESSION, &FAST).await;
        assert!(moves(&closed).is_empty(), "a node the user closed stays closed");
    }

    #[tokio::test]
    async fn no_session_is_brought_back_while_usage_is_exhausted_and_it_is_when_usage_is_back() {
        let (_home, domain) = world(true);
        domain.live.usage_pause.exhaust(Some(alondra_core::now_millis() + 3_600_000), alondra_core::now_millis());
        let watching = tokio::spawn({
            let domain = domain.clone();
            async move { revive(&domain, SESSION, &FAST).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(moves(&domain).is_empty(), "a session brought back would only hit the limit again");
        assert!(!watching.is_finished());
        domain.live.usage_pause.resume(alondra_core::now_millis());
        watching.await.unwrap();
        brought_back_told(&domain, CARRY_ON);
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
        assert!(domain.live.is_closed(SESSION) && domain.live.closed_sessions() == [SESSION], "the node closes");
        let closed = domain.live.closed_task("t").expect("the relaunch knows the node that was closed");
        assert_eq!(closed.name, "alondra/probe");
        assert!(state.interactions.is_empty(), "no card asks whether to retry");
    }

    #[tokio::test]
    async fn a_node_the_user_stopped_in_claude_agents_is_not_brought_back_and_its_task_is_freed() {
        let (home, domain) = world(true);
        working(&domain);
        job_folder(&home, &domain, "stopped");
        revive(&domain, SESSION, &FAST).await;
        assert!(moves(&domain).is_empty(), "no move to the SDK");
        freed(&domain);
        assert!(!domain.live.mods.connected(SESSION));
    }

    #[tokio::test]
    async fn a_node_the_user_stopped_while_usage_is_exhausted_frees_its_task_without_waiting_for_usage() {
        let (home, domain) = world(true);
        working(&domain);
        job_folder(&home, &domain, "stopped");
        domain.live.usage_pause.exhaust(Some(alondra_core::now_millis() + 3_600_000), alondra_core::now_millis());
        tokio::time::timeout(Duration::from_secs(2), revive(&domain, SESSION, &FAST)).await
            .expect("a stopped session does not wait for the usage limit to clear");
        assert!(moves(&domain).is_empty());
        freed(&domain);
    }

    #[tokio::test]
    async fn a_node_the_user_deleted_from_claude_agents_is_not_brought_back_and_its_task_is_freed() {
        let (home, domain) = world(true);
        working(&domain);
        let dir = job_folder(&home, &domain, "idle");
        std::fs::remove_dir_all(dir).unwrap();
        revive(&domain, SESSION, &FAST).await;
        assert!(moves(&domain).is_empty());
        freed(&domain);
    }

    #[tokio::test]
    async fn a_node_that_crashed_is_still_brought_back_and_keeps_its_task() {
        let (home, domain) = world(true);
        working(&domain);
        job_folder(&home, &domain, "crashed");
        revive(&domain, SESSION, &FAST).await;
        brought_back_told(&domain, CARRY_ON);
        let state = domain.state().unwrap();
        assert_eq!(state.tasks[&"t".into()].agent_id, Some(SESSION.into()), "a signal is not the user's choice");
        assert!(!domain.live.is_closed(SESSION) && domain.live.closed_task("t").is_none());
    }

    #[tokio::test]
    async fn a_stopped_node_with_no_open_task_just_drops_out_as_before() {
        let (home, domain) = world(false);
        job_folder(&home, &domain, "stopped");
        revive(&domain, SESSION, &FAST).await;
        assert!(moves(&domain).is_empty());
        assert!(!domain.live.is_closed(SESSION) && domain.live.closed_task_ids().is_empty(), "nothing is marked or kept for it");
    }

    /// The node's task started, then the orchestrator made it wait for another task that is still
    /// open, and the node's turn ended: «mapa del backend», 16 October 14:40.
    fn waiting(domain: &DomainRuntime) {
        command(domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "r".into(), title: "Backend map".into() });
        command(domain, Actor::Orchestrator { generation: 0 }, "dependency", Command::CreateTask { plan_items: None, task_id: "d".into(), request_id: "r".into(),
            title: "Remove the legacy code".into(), agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
        working(domain);
        crate::domain_agent_lifecycle::node_event(domain, &AgentId(SESSION.into()), NodeEvent::TurnStarted).unwrap();
        set_dependencies(domain, &["d"]);
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].state, TaskState::WaitingDependency);
        crate::domain_agent_lifecycle::node_event(domain, &AgentId(SESSION.into()), NodeEvent::TurnEnded).unwrap();
        assert_eq!(phase(domain), NodePhase::WaitingDependency);
    }

    fn phase(domain: &DomainRuntime) -> NodePhase {
        domain.state().unwrap().agents[&AgentId(SESSION.into())].phase.clone()
    }

    fn set_dependencies(domain: &DomainRuntime, on: &[&str]) {
        command(domain, Actor::Orchestrator { generation: 0 }, &format!("depend-{}", on.join("-")), Command::SetDependencies {
            task_id: "t".into(), dependencies: on.iter().map(|id| (*id).into()).collect(), stack: None,
        });
    }

    fn activity(domain: &DomainRuntime) -> Option<alondra_core::domain::TaskActivity> {
        let state = domain.state().unwrap();
        state.task_activity(&state.tasks[&"t".into()])
    }

    /// 16 October, 15:30: the session of a node whose task waits for other tasks ended. Alondra
    /// queued it at the gate to come back and restarted it at 17:32, its dependencies still open.
    #[tokio::test]
    async fn a_session_that_ends_while_its_task_waits_for_other_tasks_is_not_restarted() {
        let (_home, domain) = world(true);
        waiting(&domain);
        crate::domain_agent_lifecycle::node_event(&domain, &AgentId(SESSION.into()), NodeEvent::SessionLost).unwrap();
        assert_eq!(phase(&domain), NodePhase::Lost);
        assert_eq!(activity(&domain), Some(alondra_core::domain::TaskActivity::WaitingDependency), "its task waits for another task, not to come back");
        revive(&domain, SESSION, &FAST).await;
        assert!(moves(&domain).is_empty(), "no move to the SDK while it waits");
        assert_eq!(phase(&domain), NodePhase::Lost);
        assert!(!domain.live.mods.connected(SESSION));
    }

    /// 16 October 22:41, the daemon restarted and reported every known session as lost; 17 October
    /// 08:35 and 19 October 13:05, the mod's connection was gone: each time a node whose task waits
    /// for other tasks, still resting on them, was restarted.
    #[tokio::test]
    async fn a_node_resting_on_other_tasks_with_no_connection_is_not_restarted() {
        let (_home, domain) = world(true);
        waiting(&domain);
        assert!(!domain.live.mods.connected(SESSION));
        revive(&domain, SESSION, &FAST).await;
        assert!(moves(&domain).is_empty());
        assert_eq!(phase(&domain), NodePhase::WaitingDependency);
    }

    /// 17 October, 08:37: a minute after its turn ended, Claude Code listed the session working.
    /// The node read as at work, its silent turn as stalled, and at 08:39 it was restarted and
    /// told it had stopped in the middle of a turn.
    #[tokio::test]
    async fn a_node_resting_on_other_tasks_is_never_a_stalled_turn() {
        let (_home, domain) = world(true);
        waiting(&domain);
        let _link = halted_turn(&domain);
        crate::domain_agent_lifecycle::node_event(&domain, &AgentId(SESSION.into()), NodeEvent::Listed { listed: alondra_core::domain::Listed::Working }).unwrap();
        assert_eq!(phase(&domain), NodePhase::WaitingDependency, "a listing does not start its turn");
        assert!(stalled_sessions(&domain).is_empty());
        revive_stalled(&domain, SESSION).await;
        assert!(moves(&domain).is_empty());
    }

    /// Once the tasks it waited for are done, the node whose session ended meanwhile is brought
    /// back by that fact, through the gate, and told to carry on.
    #[tokio::test]
    async fn a_node_whose_wait_ends_is_brought_back_once() {
        let (_home, domain) = world(true);
        let mut lost = domain.live.losses.take().unwrap();
        waiting(&domain);
        crate::domain_agent_lifecycle::node_event(&domain, &AgentId(SESSION.into()), NodeEvent::SessionLost).unwrap();
        let watching = tokio::spawn(wake_when_waits_end(domain.clone()));
        tokio::task::yield_now().await;
        set_dependencies(&domain, &[]);
        let reported = tokio::time::timeout(Duration::from_secs(5), lost.recv()).await.expect("the end of the wait reports the node");
        assert_eq!(reported.as_deref(), Some(SESSION));
        watching.abort();
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].state, TaskState::Running);
        revive(&domain, SESSION, &FAST).await;
        brought_back_told(&domain, CARRY_ON);
    }
}
