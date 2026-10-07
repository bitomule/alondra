use super::*;
use crate::domain_store::model_metrics::{Decision, DecisionKind};
use crate::node_close::Answer;
use alondra_core::domain::{
    Actor, CommandEnvelope, DeliveryTarget, ModelChooser, PROTOCOL_VERSION,
};
use alondra_core::{DaemonObservation, Event};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// 2026-01-02T00:00:00Z.
fn now() -> i64 {
    crate::watcher::parse_rfc3339_millis("2026-01-02T00:00:00.000Z").unwrap()
}

struct FakeSessions {
    listed: Mutex<Vec<String>>,
    leftovers: Mutex<BTreeMap<String, String>>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl FakeSessions {
    fn removals(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().filter(|call| call[0] == "rm").map(|call| call[1].clone()).collect()
    }
}

impl Sessions for FakeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(args.clone());
            match args[0].as_str() {
                "agents" => Ok(json!(self.listed.lock().unwrap().iter().map(|id| json!({"id": id})).collect::<Vec<_>>()).to_string()),
                "rm" => {
                    self.listed.lock().unwrap().retain(|id| id != &args[1]);
                    Ok(format!("removed {}", args[1]))
                }
                _ => Err("unexpected".into()),
            }
        })
    }

    fn leftovers<'a>(&'a self, session_id: &'a str) -> Answer<'a, Option<String>> {
        Box::pin(async move { self.leftovers.lock().unwrap().get(session_id).cloned() })
    }
}

struct Rig {
    root: std::path::PathBuf,
    claude_home: std::path::PathBuf,
    domain: Arc<DomainRuntime>,
    sessions: Arc<FakeSessions>,
    count: usize,
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    command_as(domain, actor, &format!("c-{}", crate::auth::random_hex(8)), command);
}

fn command_as(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch, command_id: id.into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

#[derive(Clone, Copy, PartialEq)]
enum Task {
    Done,
    Cancelled,
    Open,
    None,
}

impl Rig {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("alondra-cleanup-{}", crate::auth::random_hex(8)));
        let home = root.join("home");
        let claude_home = root.join("claude");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(claude_home.join("jobs")).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false });
        command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
        let sessions = Arc::new(FakeSessions { listed: Default::default(), leftovers: Default::default(), calls: Default::default() });
        Self { root, claude_home, domain, sessions, count: 0 }
    }

    /// A session with a task in the given state; `launched` says whether Alondra started it.
    /// Returns the session id.
    fn session(&mut self, task: Task, launched: bool) -> String {
        self.count += 1;
        let n = self.count;
        let session = format!("a{n:07}-0000-4000-8000-000000000000");
        let short = format!("a{n:07}");
        command(&self.domain, Actor::System, Command::RegisterAgent { agent_id: session.clone().into(), name: format!("Node {n}") });
        self.domain.live.apply(&Event::DaemonSeen(DaemonObservation {
            session_id: session.clone(), short_id: short.clone(), name: Some(format!("Node {n}")), ..Default::default()
        }));
        self.domain.live.observe(&serde_json::json!({"session_id": session, "hook_event_name": "SessionStart"}));
        self.domain.live.observe(&serde_json::json!({"session_id": session, "hook_event_name": "Stop"}));
        self.sessions.listed.lock().unwrap().push(short);
        let id = format!("t{n}");
        command(&self.domain, user(), Command::CreateTask { task_id: id.clone().into(), request_id: "r".into(), title: format!("Task {n}"),
            agent_id: Some(session.clone().into()), review_prs: false, dependencies: Default::default(), stack: None });
        if launched {
            self.domain.record_model_decision(&Decision {
                task_id: id.clone(), request_id: "r".into(), agent_id: Some(session.clone()), at: 1, kind: DecisionKind::Launch,
                model: None, effort: None, approach: None, chosen_by: ModelChooser::User, reason: None, title: format!("Task {n}"),
            }).unwrap();
        }
        match task {
            Task::Done => {
                let targets = vec![DeliveryTarget::Text { text: "Result".into() }];
                command(&self.domain, Actor::Agent { agent_id: session.clone().into() }, Command::PresentResult {
                    attachments: vec![], task_id: id.clone().into(), interaction_id: format!("final-{n}").into(), body: "Done".into(),
                    targets: targets.clone(), is_final: true, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
                command(&self.domain, user(), Command::AcceptDelivery { presentation_id: format!("final-{n}").into(), targets });
                command(&self.domain, Actor::System, Command::RecordNodeEnd { task_id: id.clone().into(), end: NodeEnd::Closed, agent_id: None });
                self.forget_wrap_up(&id);
            }
            Task::Cancelled => {
                command(&self.domain, user(), Command::CancelTask { task_id: id.clone().into() });
                self.forget_wrap_up(&id);
            }
            Task::None => {
                command(&self.domain, Actor::System, Command::ReleaseTask { task_id: id.clone().into(), reason: "node closed by the user".into() });
            }
            Task::Open => {}
        }
        session
    }

    /// The task ended before wrap-ups existed: its node end was never recorded.
    fn forget_wrap_up(&self, task: &str) {
        self.domain.store.lock().unwrap().conn_for_test().execute("DELETE FROM task_wrap_ups WHERE task_id=?1", [task]).unwrap();
    }

    fn job(&self, session: &str, state: &str, updated: &str) {
        let short = &session[..8];
        std::fs::create_dir_all(self.claude_home.join("jobs").join(short)).unwrap();
        std::fs::write(self.claude_home.join("jobs").join(short).join("state.json"),
            format!(r#"{{"state":"{state}","detail":"d","sessionId":"{session}","updatedAt":"{updated}"}}"#)).unwrap();
    }

    async fn run(&self, dry_run: bool) -> Value {
        run(&self.domain, self.sessions.as_ref(), &self.claude_home, dry_run, now()).await.unwrap()
    }
}

fn names(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap().to_owned()).collect()
}

#[tokio::test]
async fn a_dry_run_lists_what_it_would_close_and_closes_nothing() {
    let mut rig = Rig::new();
    rig.session(Task::Done, true);
    rig.session(Task::Cancelled, true);
    let report = rig.run(true).await;
    assert_eq!(report["dry_run"], true);
    assert_eq!(names(&report["close"]), ["Node 1", "Node 2"]);
    assert_eq!(report["close"][0]["why"], "task_done");
    assert_eq!(report["close"][0]["task"], "Task 1");
    assert_eq!(report["close"][1]["why"], "task_cancelled");
    assert!(report["closed"].as_array().unwrap().is_empty());
    assert!(rig.sessions.removals().is_empty(), "no claude rm");
}

#[tokio::test]
async fn cleanup_closes_sessions_of_finished_tasks_and_ones_stopped_without_a_task_and_records_them() {
    let mut rig = Rig::new();
    let done = rig.session(Task::Done, true);
    let cancelled = rig.session(Task::Cancelled, true);
    let idle = rig.session(Task::None, true);
    rig.job(&idle, "idle", "2025-12-25T00:00:00.000Z");
    let report = rig.run(false).await;
    assert_eq!(report["dry_run"], false);
    assert_eq!(names(&report["closed"]), ["Node 1", "Node 2", "Node 3"]);
    assert_eq!(report["closed"][2]["why"], "stopped_without_task");
    assert_eq!(report["closed"][2]["task"], Value::Null);
    assert_eq!(rig.sessions.removals(), ["a0000001", "a0000002", "a0000003"]);
    let state = rig.domain.state().unwrap();
    for (task, end) in [("t1", NodeEnd::Closed), ("t2", NodeEnd::Closed)] {
        assert_eq!(state.tasks[&alondra_core::domain::TaskId::from(task)].wrap_up.node, Some(end), "{task} remembers its node was closed");
    }
    let _ = (done, cancelled);
    let again = rig.run(false).await;
    assert!(again["closed"].as_array().unwrap().is_empty() && again["kept"].as_array().unwrap().is_empty(), "a second run finds nothing");
}

#[tokio::test]
async fn cleanup_keeps_sessions_with_unsaved_work_working_or_waiting_and_says_why() {
    let mut rig = Rig::new();
    let unsaved = rig.session(Task::Done, true);
    let working = rig.session(Task::Done, true);
    let waiting = rig.session(Task::Cancelled, true);
    rig.sessions.leftovers.lock().unwrap().insert(unsaved, "a.txt is not committed".into());
    rig.domain.live.observe(&serde_json::json!({"session_id": working, "hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    rig.domain.live.observe(&serde_json::json!({"session_id": waiting, "hook_event_name": "PermissionRequest"}));
    let report = rig.run(false).await;
    assert!(rig.sessions.removals().is_empty());
    let kept: Vec<(String, String)> = report["kept"].as_array().unwrap().iter()
        .map(|row| (row["name"].as_str().unwrap().to_owned(), row["reason"].as_str().unwrap().to_owned())).collect();
    assert_eq!(kept, [("Node 1".to_owned(), "worktree".to_owned()), ("Node 2".to_owned(), "busy".to_owned()), ("Node 3".to_owned(), "permission".to_owned())]);
    assert_eq!(report["kept"][0]["detail"], "a.txt is not committed");
}

#[tokio::test]
async fn cleanup_never_looks_at_open_tasks_the_orchestrator_or_sessions_alondra_did_not_launch() {
    let mut rig = Rig::new();
    rig.session(Task::Open, true);
    let foreign = rig.session(Task::Done, false);
    let orchestrator = rig.session(Task::None, true);
    rig.job(&orchestrator, "idle", "2025-12-25T00:00:00.000Z");
    rig.domain.live.apply(&Event::MarkedOrchestrator { session_id: orchestrator });
    let report = rig.run(false).await;
    let _ = foreign;
    for list in ["close", "closed", "kept", "failed"] {
        assert!(report[list].as_array().unwrap().is_empty(), "{list}: {report}");
    }
    assert_eq!(rig.sessions.calls.lock().unwrap().iter().filter(|call| call[0] == "rm").count(), 0);
}

#[tokio::test]
async fn a_session_without_a_task_stopped_less_than_a_day_ago_is_not_closed() {
    let mut rig = Rig::new();
    let recent = rig.session(Task::None, true);
    let working = rig.session(Task::None, true);
    let old = rig.session(Task::None, true);
    rig.job(&recent, "idle", "2026-01-01T12:00:00.000Z");
    rig.job(&working, "working", "2025-12-01T00:00:00.000Z");
    rig.job(&old, "idle", "2025-12-25T00:00:00.000Z");
    let report = rig.run(false).await;
    assert_eq!(names(&report["closed"]), ["Node 3"], "only the one stopped for more than a day: {report}");
    assert_eq!(rig.sessions.removals(), ["a0000003"]);
}

#[tokio::test]
async fn when_claude_agents_cannot_run_nothing_is_closed() {
    struct Broken;
    impl Sessions for Broken {
        fn claude<'a>(&'a self, _: Vec<String>) -> Answer<'a, Result<String, String>> {
            Box::pin(async { Err("claude is not installed".into()) })
        }
        fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> {
            Box::pin(async { None })
        }
    }
    let rig = Rig::new();
    let refused = run(&rig.domain, &Broken, &rig.claude_home, false, now()).await.unwrap_err();
    assert_eq!((refused.0, refused.1.as_str()), (StatusCode::BAD_GATEWAY, "claude is not installed"));
}

impl Rig {
    /// A session registered and listed, with a cancelled task given to it the way `launch` did before
    /// decisions were kept: no `model_decisions` row at all.
    fn old_session(&mut self, assign_as: Option<Actor>, launch_key: Option<&str>) -> String {
        self.old_session_with(assign_as, "orchestrator-tool-launch", launch_key)
    }

    fn old_session_with(&mut self, assign_as: Option<Actor>, assign_id: &str, launch_key: Option<&str>) -> String {
        self.count += 1;
        let n = self.count;
        let session = format!("a{n:07}-0000-4000-8000-000000000000");
        let short = format!("a{n:07}");
        command(&self.domain, Actor::System, Command::RegisterAgent { agent_id: session.clone().into(), name: format!("Node {n}") });
        self.domain.live.apply(&Event::DaemonSeen(DaemonObservation {
            session_id: session.clone(), short_id: short.clone(), name: Some(format!("Node {n}")), ..Default::default()
        }));
        self.domain.live.observe(&serde_json::json!({"session_id": session, "hook_event_name": "SessionStart"}));
        self.domain.live.observe(&serde_json::json!({"session_id": session, "hook_event_name": "Stop"}));
        if let Some(key) = launch_key {
            self.domain.live.mods.loaded_for(&session, Some(key));
        }
        self.sessions.listed.lock().unwrap().push(short);
        let id = format!("t{n}");
        let given = assign_as.is_some();
        command(&self.domain, user(), Command::CreateTask { task_id: id.clone().into(), request_id: "r".into(), title: format!("Task {n}"),
            agent_id: (!given).then(|| session.clone().into()), review_prs: false, dependencies: Default::default(), stack: None });
        if let Some(actor) = assign_as {
            command_as(&self.domain, actor, assign_id, Command::AssignTask { task_id: id.clone().into(), agent_id: session.clone().into(), model: None });
        }
        command(&self.domain, user(), Command::CancelTask { task_id: id.clone().into() });
        self.forget_wrap_up(&id);
        session
    }
}

#[tokio::test]
async fn sessions_from_before_launch_decisions_were_kept_still_count_as_alondras() {
    let mut rig = Rig::new();
    rig.old_session(Some(Actor::Orchestrator { generation: 0 }), None);
    rig.old_session(Some(Actor::System), None);
    rig.old_session(None, Some("launch-1"));
    rig.old_session(None, None);
    rig.old_session(Some(user()), None);
    assert!(rig.domain.store.lock().unwrap().model_decision_rows().unwrap().is_empty(), "no decision was ever recorded");
    let report = rig.run(true).await;
    assert_eq!(names(&report["close"]), ["Node 1", "Node 2", "Node 3"], "the user's own sessions are not listed: {report}");
}

#[tokio::test]
async fn a_node_the_user_left_open_and_a_task_in_progress_are_not_closed() {
    let mut rig = Rig::new();
    let kept = rig.session(Task::Cancelled, true);
    let open = rig.session(Task::Open, true);
    let _ = (&kept, &open);
    rig.domain.store.lock().unwrap().conn_for_test().execute("INSERT OR REPLACE INTO task_wrap_ups(task_id,close_cancelled,node_end) VALUES('t1',0,'kept_by_user')", []).unwrap();
    rig.domain.state().unwrap();
    let report = rig.run(true).await;
    for list in ["close", "kept", "failed"] {
        assert!(report[list].as_array().unwrap().is_empty(), "{list}: {report}");
    }
}

#[tokio::test]
async fn a_session_of_the_users_that_the_orchestrator_gave_a_task_with_assign_task_is_never_closed() {
    let mut rig = Rig::new();
    rig.old_session_with(Some(Actor::Orchestrator { generation: 0 }), "mcp-assign-task", None);
    rig.old_session_with(Some(Actor::System), "mcp-assign-task", None);
    let launched = rig.old_session(Some(Actor::Orchestrator { generation: 0 }), None);
    let dry = rig.run(true).await;
    assert_eq!(names(&dry["close"]), ["Node 3"], "only Alondra's launch path counts: {dry}");
    let report = rig.run(false).await;
    assert_eq!(rig.sessions.removals(), [launched[..8].to_owned()], "the user's sessions got no claude rm: {report}");
}

#[tokio::test]
async fn cleanup_right_after_the_daemon_starts_closes_finished_sessions_whose_mod_has_not_connected() {
    let mut rig = Rig::new();
    rig.session(Task::Done, true);
    rig.session(Task::Cancelled, true);
    let starting = tokio::spawn(crate::node_revival::run(rig.domain.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let report = rig.run(false).await;
    starting.abort();
    assert_eq!(names(&report["closed"]), ["Node 1", "Node 2"], "{report}");
    assert!(report["kept"].as_array().unwrap().is_empty(), "{report}");
}
