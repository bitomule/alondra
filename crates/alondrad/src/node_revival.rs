//! Brings a node back when its connection is gone while its task is still open. A node does not
//! simply disconnect: its Claude Code background session is restarted with `claude respawn <id>`
//! ("Restart a background session ... so it picks up the current Claude binary", `claude respawn
//! --help`, Claude Code 2.1.287), a bounded number of times, and its mod's `session.start`
//! reconnects it. Fleet shows the node's last state throughout, and "Reconnecting" only while a
//! restart runs. When restarts keep failing the user gets a card with the error, not a node state.
//!
//! A session the user stops, kills or deletes in Claude agents is not a crash: Claude Code writes
//! the state `stopped` into its job file (`crashed` for a signal), or removes the job folder, and
//! restarting it would undo what the user chose. Such a node leaves Fleet and its task goes back
//! to not started, waiting for a relaunch.
use crate::domain_runtime::DomainRuntime;
use crate::orchestrate::Launcher;
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, QuestionOption, PROTOCOL_VERSION};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const RETRY: &str = "retry";
const STALL_CHECK: Duration = Duration::from_secs(30);
const CONTINUE: &str = "Alondra restarted your session because it had stopped processing in the middle of a turn. Check where you were and continue your task.";

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

pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    let Some(mut lost) = domain.live.losses.take() else { return };
    if let Ok(state) = domain.state() {
        for agent in state.agents.keys() {
            domain.live.losses.report(&agent.0);
        }
    }
    tokio::spawn(watch_stalls(domain.clone()));
    while let Some(session) = lost.recv().await {
        let domain = domain.clone();
        tokio::spawn(async move { revive(&domain, &session, &crate::orchestrate::ClaudeCli, &Timing::LIVE).await });
    }
}

async fn watch_stalls(domain: Arc<DomainRuntime>) {
    loop {
        tokio::time::sleep(STALL_CHECK).await;
        look_at_shells(&domain);
        for session in stalled_sessions(&domain, alondra_core::now_millis()) {
            let domain = domain.clone();
            tokio::spawn(async move { revive_stalled(&domain, &session, &crate::orchestrate::ClaudeCli, &Timing::LIVE).await });
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
        domain.live.mods.set_shell_alive(&agent.0, crate::process_watch::shell_running(&table, pid));
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
fn closed_by_user(domain: &DomainRuntime, session: &str) -> bool {
    let known = domain.live.mods.session(session).and_then(|reported| reported.job_dir);
    let Some(dir) = known.or_else(|| crate::job_state::locate(&crate::watcher::claude_home(), session)) else { return false };
    !dir.exists() || crate::job_state::read_in(&dir, session).is_some_and(|job| job.phase == crate::job_state::Phase::Stopped)
}

/// The sessions whose agent works on an open task and whose mod is connected, yet shows no
/// activity inside a turn: the session stopped processing and nothing will resume it. A session
/// with a held card open waits for the user: restarting it would cut the call and withdraw the
/// card, and the node would put the same card to the user again.
pub(crate) fn stalled_sessions(domain: &DomainRuntime, now: alondra_core::Millis) -> Vec<String> {
    let Ok(state) = domain.state() else { return vec![] };
    state.agents.keys()
        .map(|agent| agent.0.clone())
        .filter(|session| open_task(&state, session) && !domain.live.is_closed(session))
        .filter(|session| !crate::domain_holds::waits_on_user(&state, session))
        .filter(|session| domain.live.mods.session(session).is_some_and(|reported| reported.silent(now)))
        .collect()
}

pub(crate) fn has_open_task(domain: &DomainRuntime, session: &str) -> bool {
    domain.state().is_ok_and(|state| open_task(&state, session))
}

fn open_task(state: &alondra_core::domain::DomainState, session: &str) -> bool {
    let agent = AgentId(session.to_owned());
    state.agents.contains_key(&agent) && state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && !task.archived)
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
            if !still_stalled(domain, session) {
                break 'rounds;
            }
            let previous = domain.live.mods.session(session).and_then(|reported| reported.connection);
            domain.live.set_reconnecting(session, true);
            match launcher.respawn(&short).await {
                Ok(()) if domain.live.mods.wait_reconnected(session, previous, timing.settle).await => {
                    tell_to_continue(domain, session).await;
                    break 'rounds;
                }
                Ok(()) => error = "the restarted session did not connect back to Alondra".into(),
                Err(reason) => error = reason,
            }
            domain.live.set_reconnecting(session, false);
        }
        if !still_stalled(domain, session) || !ask_to_retry(domain, session, &error).await {
            break;
        }
    }
    domain.live.set_reconnecting(session, false);
    domain.live.end_watch(session);
}

/// The mod's connection is recorded a moment before its push link attaches.
async fn tell_to_continue(domain: &DomainRuntime, session: &str) {
    for _ in 0..250 {
        if domain.live.pushes.message(session, CONTINUE, false).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn still_stalled(domain: &DomainRuntime, session: &str) -> bool {
    stalled_sessions(domain, alondra_core::now_millis()).iter().any(|stalled| stalled == session)
}

/// Whether the session still has to be brought back: its agent works on an open task and its mod
/// holds no connection.
fn needed(domain: &DomainRuntime, session: &str) -> bool {
    let Ok(state) = domain.state() else { return false };
    let agent = AgentId(session.to_owned());
    state.agents.contains_key(&agent)
        && state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && !task.archived)
        && !domain.live.is_closed(session)
        && !domain.live.mods.connected(session)
}

pub(crate) async fn revive(domain: &DomainRuntime, session: &str, launcher: &impl Launcher, timing: &Timing) {
    if !domain.live.begin_watch(session) {
        return;
    }
    if !domain.live.mods.wait_connected(session, timing.grace).await && needed(domain, session) {
        restart_until_back(domain, session, launcher, timing).await;
    }
    domain.live.set_reconnecting(session, false);
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
            domain.live.usage_pause.until_clear().await;
            if !needed(domain, session) {
                return;
            }
            if closed_by_user(domain, session) {
                if let Err(failure) = user_closed(domain, session) {
                    log!("could not record that {session} was closed by the user: {failure:#}");
                }
                return;
            }
            domain.live.set_reconnecting(session, true);
            match launcher.respawn(&short).await {
                Ok(()) if domain.live.mods.wait_connected(session, timing.settle).await => return,
                Ok(()) => error = "the restarted session did not connect back to Alondra".into(),
                Err(reason) => error = reason,
            }
            domain.live.set_reconnecting(session, false);
        }
        if !needed(domain, session) || !ask_to_retry(domain, session, &error).await {
            return;
        }
    }
}

/// The user closed the session: the node leaves Fleet, is not brought back, and the tasks it had
/// open go back to not started with no node, to be relaunched. What a relaunch needs of the node
/// is kept.
pub(crate) fn user_closed(domain: &DomainRuntime, session: &str) -> anyhow::Result<()> {
    let agent = AgentId(session.to_owned());
    let state = domain.state()?;
    let node = state.agents.get(&agent).map(|known| crate::client_routes::ClosedNode {
        name: known.name.clone(),
        cwd: domain.live.mods.session(session).and_then(|reported| reported.cwd),
    });
    let released: Vec<_> = state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && !task.archived && task.final_presentation.is_none())
        .map(|task| task.id.clone())
        .collect();
    for task_id in released {
        let state = domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch, command_id: format!("release-{}-{}", task_id.0, crate::auth::random_hex(8)).into(),
            expected_revision: state.revision, command: Command::ReleaseTask { task_id: task_id.clone(), reason: "node closed by the user".into() },
        };
        match domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            Ok(_) => {
                if let Some(node) = &node {
                    domain.live.remember_closed_task(&task_id.0, node.clone());
                }
            }
            Err(failure) => log!("could not free {task_id} after its node was closed: {failure:?}"),
        }
    }
    let event_id = format!("close-{session}-{}", crate::auth::random_hex(8));
    let event = crate::domain_agent_lifecycle::prepare_event(domain, &event_id).map_err(|failure| anyhow::anyhow!("{failure}"))?;
    crate::domain_agent_lifecycle::observe_instance(domain, &agent, false, &event).map_err(|failure| anyhow::anyhow!("{failure}"))?;
    crate::domain_transport::session_ended(domain, &agent)?;
    domain.live.mark_closed(session);
    domain.live.apply(&alondra_core::Event::NodeRemoved { session_id: session.to_owned() });
    Ok(())
}

/// The card for the user once restarts keep failing; true when they ask for another round.
async fn ask_to_retry(domain: &DomainRuntime, session: &str, error: &str) -> bool {
    let name = domain.state().ok().and_then(|state| state.agents.get(&AgentId(session.to_owned())).map(|agent| agent.name.clone()))
        .unwrap_or_else(|| session.to_owned());
    let body = format!("{name} is not responding to Alondra while its task is open, and restarting its session with `claude respawn` failed: {error}");
    let options = vec![
        QuestionOption { id: RETRY.into(), label: "Try again".into(), description: None, inputs: vec![] },
        QuestionOption { id: "leave".into(), label: "Leave it".into(), description: None, inputs: vec![] },
    ];
    let card = match crate::domain_holds::ask(domain, session, body, options) {
        Ok(Some(card)) => card,
        Ok(None) => return false,
        Err(failure) => {
            log!("could not tell the user that {name} could not be restarted: {failure:#}");
            return false;
        }
    };
    matches!(crate::domain_holds::wait(domain, &card).await, Ok(Some(answers)) if answers[0].option_id == RETRY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, InteractionId};
    use serde_json::json;
    use std::sync::Mutex;

    const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";
    const FAST: Timing = Timing { grace: Duration::from_millis(30), settle: Duration::from_millis(60), pauses: [Duration::from_millis(5), Duration::from_millis(5)] };

    struct Home(std::path::PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: state.data_epoch, command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn hello() -> crate::node_reports::Hello {
        serde_json::from_value(json!({"session_id": SESSION, "version": "2.1.287"})).unwrap()
    }

    /// A node whose mod connected once and then lost its connection.
    fn world(with_task: bool) -> (Home, Arc<DomainRuntime>) {
        let home = std::env::temp_dir().join(format!("alondra-revival-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let user = || Actor::User { user_id: "u".into() };
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: SESSION.into(), name: "alondra/probe".into() });
        if with_task {
            command(&domain, user(), "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false });
            command(&domain, user(), "task", Command::CreateTask { task_id: "t".into(), request_id: "r".into(), title: "Probe".into(),
                agent_id: Some(SESSION.into()), review_prs: false, dependencies: Default::default(), stack: None });
        }
        let connection = domain.live.mods.connect(hello(), 1);
        domain.live.mods.disconnect(SESSION, connection);
        (Home(home), domain)
    }

    struct Respawner {
        domain: Arc<DomainRuntime>,
        calls: Mutex<Vec<String>>,
        works_from: usize,
    }

    impl Respawner {
        fn new(domain: &Arc<DomainRuntime>, works_from: usize) -> Self {
            Self { domain: domain.clone(), calls: Mutex::new(vec![]), works_from }
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl Launcher for Respawner {
        async fn run(&self, _: &str, _: &[String]) -> Result<String, crate::orchestrate::Refusal> {
            unreachable!("a revival never launches")
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
            self.domain.live.mods.connect(hello(), 2);
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
        let claude = Respawner::new(&domain, 1);
        revive(&domain, SESSION, &claude, &FAST).await;
        assert_eq!(*claude.calls.lock().unwrap(), ["ab1234cd"], "`claude respawn` takes the short id");
        assert!(domain.live.mods.connected(SESSION), "the restarted session's mod connected back");
        assert!(!domain.live.is_reconnecting(SESSION), "and nothing says it is reconnecting any more");
        assert!(domain.state().unwrap().interactions.is_empty(), "a restart that works asks the user nothing");
    }

    const QUIET_FOR: i64 = alondra_core::states::TURN_QUIET_MS + 60_000;

    /// A node whose mod is connected and whose turn started a while ago, with nothing since.
    fn silent_turn(domain: &DomainRuntime) -> tokio::sync::mpsc::UnboundedReceiver<String> {
        let long_ago = alondra_core::now_millis() - QUIET_FOR;
        domain.live.mods.connect(hello(), long_ago);
        let link = domain.live.pushes.attach(SESSION, 90);
        domain.live.mods.sample_cpu(SESSION, 500, long_ago);
        domain.live.mods.sample_cpu(SESSION, 500, long_ago + 1);
        domain.live.mods.apply(serde_json::from_value(json!({"session_id": SESSION, "event": "turn.start", "turn_id": "t"})).unwrap(), long_ago);
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
        assert!(!domain.live.is_reconnecting(SESSION));
        assert!(domain.state().unwrap().interactions.is_empty());
    }

    /// After a daemon restart Alondra no longer knows the node is inside its plan call, yet the
    /// plan's card is still open in the domain: the node waits for the user and is left alone.
    #[tokio::test]
    async fn a_silent_session_whose_held_card_is_open_waits_for_the_user_and_is_never_restarted() {
        let (_home, domain) = world(true);
        let _link = silent_turn(&domain);
        let card = serde_json::from_value(json!({"session_id": SESSION, "id": "call-1", "kind": "plan", "input": {"plan": "Ship it"}})).unwrap();
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
        domain.live.mods.apply(serde_json::from_value(json!({"session_id": SESSION, "event": "session.measure"})).unwrap(), alondra_core::now_millis());
        assert!(stalled_sessions(&domain, alondra_core::now_millis()).is_empty(), "any report is activity");
        let (_home, finished) = world(false);
        let _link = silent_turn(&finished);
        assert!(stalled_sessions(&finished, alondra_core::now_millis()).is_empty(), "no open task: nothing to resume");
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
        answer(&domain, card, "leave");
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
                domain.live.mods.connect(hello(), 3);
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
        let state = domain.state().unwrap();
        assert!(state.interactions[&card].body.contains("no such session"), "the error reaches the user: {}", state.interactions[&card].body);
        assert!(state.interactions[&card].body.contains("alondra/probe"));
        answer(&domain, card, "leave");
        watching.await.unwrap();
        assert_eq!(claude.calls(), 3, "leaving it stops");
        assert!(!domain.live.is_reconnecting(SESSION));
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
        assert!(!domain.live.mods.connected(SESSION) && !domain.live.is_reconnecting(SESSION));
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
