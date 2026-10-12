//! Which sessions really exist, as `claude agents` lists them: every node Alondra launched takes
//! what the listing says of it, absent included, through the node machine. It runs when the daemon
//! starts, before the gate admits anything, and every minute after, so a session that ended without
//! a word (killed, removed, the daemon down meanwhile) never keeps a slot or a place in Fleet.
use crate::domain_runtime::DomainRuntime;
use crate::node_close::Sessions;
use alondra_core::domain::{AgentId, Listed};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub(crate) const EVERY: Duration = Duration::from_secs(60);

/// What the listing says of each session, by session id; `None` when it is not the JSON list.
pub(crate) fn parse(listing: &str) -> Option<BTreeMap<String, Listed>> {
    let list = serde_json::Deserializer::from_str(listing.get(listing.find('[')?..)?).into_iter::<Vec<Value>>().next()?.ok()?;
    Some(list.iter().filter_map(|session| {
        let id = session.get("sessionId").and_then(Value::as_str)?;
        let listed = match session.get("state").and_then(Value::as_str)? {
            "working" => Listed::Working,
            "idle" => Listed::Idle,
            "blocked" => Listed::Blocked,
            _ => Listed::Absent,
        };
        Some((id.to_owned(), listed))
    }).collect())
}

/// Lists the real sessions once and hands the node machine what it says of every live node
/// Alondra launched, and of every other live node whose session is gone and that holds no open
/// task, so it closes without ever being brought back. False when the listing could not be read:
/// nothing is moved then.
pub(crate) async fn once(domain: &DomainRuntime, sessions: &dyn Sessions) -> bool {
    let listing = match sessions.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await {
        Ok(listing) => listing,
        Err(error) => {
            log!("could not list Claude's sessions to check the nodes: {error}");
            return false;
        }
    };
    let Some(sessions) = parse(&listing) else {
        log!("claude agents did not answer with its list of sessions");
        return false;
    };
    let Ok(state) = domain.state() else { return false };
    let open_task = |agent: &AgentId| state.tasks.values().any(|task| task.agent_id.as_ref() == Some(agent) && !task.state.terminal());
    let listed: BTreeMap<AgentId, Listed> = state.agents.values()
        .filter(|agent| agent.phase.live() && Some(&agent.id) != state.orchestrator_session.as_ref())
        .map(|agent| (agent, sessions.get(&agent.id.0).copied().unwrap_or(Listed::Absent)))
        .filter(|(agent, listed)| domain.launched_by_alondra(&agent.id).unwrap_or(false) || (*listed == Listed::Absent && !open_task(&agent.id)))
        .map(|(agent, listed)| (agent.id.clone(), listed))
        .collect();
    match crate::domain_agent_lifecycle::reconcile(domain, listed) {
        Ok(moved) => {
            if moved {
                domain.live.gate.poke();
            }
            true
        }
        Err(error) => {
            log!("could not bring the nodes in line with Claude's sessions: {error}");
            false
        }
    }
}

/// Every minute after the first look the daemon takes before the gate opens.
pub(crate) async fn run(domain: std::sync::Arc<DomainRuntime>, sessions: std::sync::Arc<dyn Sessions>) {
    loop {
        tokio::time::sleep(EVERY).await;
        once(&domain, sessions.as_ref()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_runtime::DomainRuntime;
    use crate::node_close::Answer;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, LaunchPlan, NodeEvent, NodePhase, ORCHESTRATOR_READER, PROTOCOL_VERSION};
    use std::sync::Mutex;

    struct Claude(Mutex<String>);

    impl Sessions for Claude {
        fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<String, String>> {
            Box::pin(async move {
                assert_eq!(args, ["agents", "--json", "--all"]);
                Ok(self.0.lock().unwrap().clone())
            })
        }
        fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> {
            Box::pin(async { None })
        }
    }

    fn run(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// A node Alondra launched on its own task, in a turn.
    fn launch(domain: &DomainRuntime, id: &str) {
        let now = alondra_core::now_millis();
        run(domain, Actor::Orchestrator { generation: 0 }, &format!("create-{id}"), Command::CreateTask {
            task_id: format!("t-{id}").into(), request_id: "request".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        });
        run(domain, Actor::System, &format!("register-{id}"), Command::RegisterAgent { agent_id: id.into(), name: id.into(), at: now });
        run(domain, Actor::System, &format!("orchestrator-tool-assign-{id}"), Command::AssignTask { task_id: format!("t-{id}").into(), agent_id: id.into(), model: None });
        run(domain, Actor::Agent { agent_id: id.into() }, &format!("start-{id}"), Command::StartTask { task_id: format!("t-{id}").into() });
        run(domain, Actor::System, &format!("turn-{id}"), Command::MoveNode { agent_id: id.into(), event: NodeEvent::TurnStarted, at: now });
    }

    fn open(home: &std::path::Path) -> DomainRuntime {
        let domain = DomainRuntime::open(home).unwrap();
        if domain.state().unwrap().requests.is_empty() {
            run(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
                request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
            });
            run(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "request".into(), title: "Work".into() });
        }
        domain
    }

    fn phase(domain: &DomainRuntime, id: &str) -> NodePhase {
        domain.state().unwrap().agents[&id.into()].phase.clone()
    }

    /// 9 October: 10 nodes read as connected, 3 sessions existed, and the ghosts held the slots
    /// until the daemon was restarted by hand.
    #[tokio::test]
    async fn a_restart_frees_the_slots_and_fleet_of_sessions_that_ended_while_nobody_watched() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        for id in ["alive", "vanished", "finished"] {
            launch(&domain, id);
        }
        run(&domain, Actor::User { user_id: "u".into() }, "cancel-finished", Command::CancelTask { task_id: "t-finished".into() });
        assert_eq!(domain.state().unwrap().slots().len(), 2);
        drop(domain);

        let domain = open(&home);
        let claude = Claude(Mutex::new(r#"[{"id":"alive","sessionId":"alive","state":"working"},{"id":"gone","sessionId":"finished","state":"done"}]"#.into()));
        assert!(once(&domain, &claude).await);
        let state = domain.state().unwrap();
        assert_eq!(state.slots().iter().map(|agent| agent.id.0.as_str()).collect::<Vec<_>>(), ["alive"], "only the session that exists works");
        assert_eq!(phase(&domain, "vanished"), NodePhase::Starting { reviving: true }, "its task is open: Alondra brings it back, holding no slot");
        assert_eq!(phase(&domain, "finished"), NodePhase::Closed, "its task is over and its session gone: it leaves Fleet");

        let revision = state.revision;
        assert!(once(&domain, &claude).await);
        assert_eq!(domain.state().unwrap().revision, revision, "the same listing again moves nothing");
    }

    /// 12 October: a scheduled run whose task was cancelled before its session started never
    /// linked the node to its launch, so the listing skipped it and it worked in Fleet for days.
    #[tokio::test]
    async fn a_node_alondra_cannot_tell_it_launched_closes_once_its_session_is_gone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        let now = alondra_core::now_millis();
        for id in ["unlinked-gone", "unlinked-alive"] {
            run(&domain, Actor::System, &format!("register-{id}"), Command::RegisterAgent { agent_id: id.into(), name: format!("scheduled/{id}"), at: now });
            run(&domain, Actor::System, &format!("turn-{id}"), Command::MoveNode { agent_id: id.into(), event: NodeEvent::TurnStarted, at: now });
        }
        assert!(!domain.launched_by_alondra(&"unlinked-gone".into()).unwrap());
        let claude = Claude(Mutex::new(r#"[{"id":"gone","sessionId":"unlinked-gone","state":"done"},{"id":"alive","sessionId":"unlinked-alive","state":"working"}]"#.into()));
        assert!(once(&domain, &claude).await);
        assert_eq!(phase(&domain, "unlinked-gone"), NodePhase::Closed);
        assert_eq!(phase(&domain, "unlinked-alive"), NodePhase::Working, "a session that exists is not Alondra's to judge");
    }

    #[tokio::test]
    async fn a_listing_that_cannot_be_read_moves_nothing() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        launch(&domain, "alive");
        assert!(!once(&domain, &Claude(Mutex::new("error: not logged in".into()))).await);
        assert_eq!(phase(&domain, "alive"), NodePhase::Working);
    }

    /// Domains written before the node machine kept `connected`: a connected node of a finished
    /// task, one of an open task whose session is gone, and one with nothing.
    #[tokio::test]
    async fn an_older_domain_takes_phases_from_its_connected_flags_and_the_first_listing_settles_them() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        for id in ["alive", "open-gone", "done-ghost"] {
            launch(&domain, id);
        }
        run(&domain, Actor::User { user_id: "u".into() }, "cancel-ghost", Command::CancelTask { task_id: "t-done-ghost".into() });
        drop(domain);
        let db = rusqlite::Connection::open(home.join("domain.db")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF;
            ALTER TABLE agents ADD COLUMN connected INTEGER NOT NULL DEFAULT 1 CHECK(connected IN(0,1));
            UPDATE agents SET connected=0 WHERE id='open-gone';
            ALTER TABLE agents DROP COLUMN phase; ALTER TABLE agents DROP COLUMN since;").unwrap();
        drop(db);

        let domain = open(&home);
        assert_eq!(phase(&domain, "alive"), NodePhase::Working);
        assert_eq!(phase(&domain, "open-gone"), NodePhase::Starting { reviving: true });
        let claude = Claude(Mutex::new(r#"[{"id":"alive","sessionId":"alive","state":"working"}]"#.into()));
        assert!(once(&domain, &claude).await);
        assert_eq!(phase(&domain, "done-ghost"), NodePhase::Closed);
        assert_eq!(domain.state().unwrap().slots().len(), 1);
    }

    /// 346 notices to the orchestrator stayed unread: nothing ever read them once handed over.
    #[test]
    fn a_notice_handed_to_the_orchestrator_is_read_and_one_still_queued_is_not() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        domain.tell_orchestrator("handed", "Task one ended".into()).unwrap();
        domain.tell_orchestrator("queued", "Task two ended".into()).unwrap();
        {
            let mut store = domain.store.lock().unwrap();
            let pending = store.pending_messages().unwrap();
            let handed = pending.iter().find(|message| message.interaction_id.as_deref() == Some("handed")).unwrap();
            let claim = store.claim_orchestrator_message(handed.id).unwrap().unwrap();
            assert!(store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap());
        }
        crate::domain_orchestrator::read_handed_notices(&domain);
        crate::domain_orchestrator::read_handed_notices(&domain);
        let state = domain.state().unwrap();
        let read = |id: &str| match &state.interactions[&id.into()].payload {
            alondra_core::domain::InteractionPayload::Information { read_by } => read_by.iter().cloned().collect::<Vec<_>>(),
            _ => panic!("information"),
        };
        assert_eq!(read("handed"), [ORCHESTRATOR_READER]);
        assert!(read("queued").is_empty(), "a notice not handed yet still waits");
    }

    #[test]
    fn the_listing_says_each_session_by_its_id_and_a_stopped_or_done_one_is_absent() {
        let listing = r#"[{"id":"9462eaef","sessionId":"9462eaef-2d59","state":"done"},{"id":"200ea49d","sessionId":"200ea49d-a71d","state":"working"},
            {"id":"316f9b7e","sessionId":"316f9b7e-3d4a","state":"idle"},{"id":"30fdb363","sessionId":"30fdb363-1965","state":"stopped"}]
            warning: something on stderr"#;
        let parsed = parse(listing).unwrap();
        assert_eq!(parsed["200ea49d-a71d"], Listed::Working);
        assert_eq!(parsed["316f9b7e-3d4a"], Listed::Idle);
        assert_eq!(parsed["9462eaef-2d59"], Listed::Absent);
        assert_eq!(parsed["30fdb363-1965"], Listed::Absent);
        assert_eq!(parse("claude: command not found"), None);
    }
}
