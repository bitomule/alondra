//! Brings a node back when its connection is gone while its task is still open. A node does not
//! simply disconnect: its Claude Code background session is restarted with `claude respawn <id>`
//! ("Restart a background session ... so it picks up the current Claude binary", `claude respawn
//! --help`, Claude Code 2.1.287), a bounded number of times, and its mod's `session.start`
//! reconnects it. Fleet shows the node's last state throughout, and "Reconnecting" only while a
//! restart runs. When restarts keep failing the user gets a card with the error, not a node state.
use crate::domain_runtime::DomainRuntime;
use crate::orchestrate::Launcher;
use alondra_core::domain_v2::{AgentId, QuestionOption};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const RETRY: &str = "retry";

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
    while let Some(session) = lost.recv().await {
        let domain = domain.clone();
        tokio::spawn(async move { revive(&domain, &session, &crate::orchestrate::ClaudeCli, &Timing::LIVE).await });
    }
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
            if !needed(domain, session) {
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

/// The card for the user once restarts keep failing; true when they ask for another round.
async fn ask_to_retry(domain: &DomainRuntime, session: &str, error: &str) -> bool {
    let name = domain.state().ok().and_then(|state| state.agents.get(&AgentId(session.to_owned())).map(|agent| agent.name.clone()))
        .unwrap_or_else(|| session.to_owned());
    let body = format!("{name} lost its connection to Alondra while its task is open, and restarting its session with `claude respawn` failed: {error}");
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
    matches!(crate::domain_holds::wait(domain, &card).await, Ok(Some(answer)) if answer.option_id == RETRY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::{Actor, Command, CommandEnvelope, InteractionId};
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
}
