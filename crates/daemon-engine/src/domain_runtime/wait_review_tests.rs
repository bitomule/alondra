//! A daemon that opens its domain ends every saved wait whose waits are over: the task goes on,
//! its row and history are saved, and its node is told. «Pebble: progreso diario» stayed waiting
//! six hours after its dependencies were done because opening the 0.3.6 daemon moved it on in
//! memory only.
use super::*;
use rusqlite::Connection;
use std::collections::BTreeSet;

struct Home(crate::test_support::Directory);

impl Home {
    fn new() -> Self {
        let directory = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        Self(directory)
    }

    fn database(&self) -> Connection {
        Connection::open(crate::domain_database_migration::active_path(&self.0)).unwrap()
    }

    /// What an older daemon saved: the task kept waiting although its dependency is over.
    fn saved_waiting(&self, task: &str) {
        self.database().execute("UPDATE tasks SET state='waiting_dependency' WHERE id=?1", [task]).unwrap();
    }

    fn row(&self, task: &str) -> String {
        self.database().query_row("SELECT state FROM tasks WHERE id=?1", [task], |row| row.get(0)).unwrap()
    }

    fn history(&self, task: &str) -> Vec<String> {
        let database = self.database();
        let mut rows = database.prepare("SELECT state FROM task_history WHERE task_id=?1 ORDER BY revision").unwrap();
        rows.query_map([task], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
    }

    fn told(&self, agent: &str) -> Vec<String> {
        let database = self.database();
        let mut rows = database.prepare("SELECT message FROM domain_messages WHERE agent_id=?1 ORDER BY id").unwrap();
        rows.query_map([agent], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
    }
}

fn must(runtime: &DomainRuntime, actor: Actor, command: Command) {
    let state = runtime.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("c-{}", state.revision).into(),
        expected_revision: state.revision,
        command,
    };
    runtime.execute(&actor, &envelope, None, || Ok(())).unwrap();
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

fn node(agent: &str) -> Actor {
    Actor::Agent { agent_id: agent.into() }
}

fn started(runtime: &DomainRuntime, task: &str, agent: &str, dependencies: &[&str]) {
    must(runtime, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    must(runtime, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: task.into(), request_id: "r".into(), title: task.into(), agent_id: Some(agent.into()), review_prs: false,
        dependencies: dependencies.iter().map(|dep| TaskId::from(*dep)).collect::<BTreeSet<_>>(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(runtime, node(agent), Command::StartTask { task_id: task.into() });
}

/// The release, whose result was accepted, and the task waiting on it, still in the release's
/// closing: what is left is the end of the release's node.
fn release_closing(home: &Home) -> DomainRuntime {
    let runtime = DomainRuntime::open(&home.0).unwrap();
    must(&runtime, orchestrator(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    started(&runtime, "release", "a", &[]);
    must(&runtime, Actor::User { user_id: "u".into() }, Command::TitleRequest { request_id: "r".into(), title: "Release".into() });
    started(&runtime, "pebble", "b", &[]);
    must(&runtime, orchestrator(), Command::SetDependencies { task_id: "pebble".into(), dependencies: BTreeSet::from(["release".into()]), stack: None });
    let text = vec![DeliveryTarget::Text { text: "Published".into() }];
    must(&runtime, node("a"), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "release".into(), interaction_id: "final".into(), body: "Published".into(),
        targets: text.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the release", &[], true),
    });
    must(&runtime, Actor::User { user_id: "u".into() }, Command::AcceptDelivery { presentation_id: "final".into(), targets: text });
    assert_eq!(runtime.state().unwrap().tasks[&TaskId::from("pebble")].state, TaskState::WaitingDependency);
    runtime
}

fn release_ends(runtime: &DomainRuntime) {
    must(runtime, Actor::System, Command::RecordNodeEnd { task_id: "release".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
}

fn told_done(home: &Home) -> usize {
    home.told("b").iter().filter(|message| message.starts_with("What you were waiting for is done")).count()
}

#[test]
fn opening_the_domain_ends_a_saved_wait_whose_dependency_is_done_and_tells_its_node() {
    let home = Home::new();
    let runtime = release_closing(&home);
    release_ends(&runtime);
    drop(runtime);
    home.saved_waiting("pebble");
    let told_before = told_done(&home);

    let runtime = DomainRuntime::open(&home.0).unwrap();

    assert_eq!(runtime.state().unwrap().tasks[&TaskId::from("pebble")].state, TaskState::Running);
    assert_eq!(home.row("pebble"), "running", "the end of the wait is saved");
    assert_eq!(home.history("pebble").last().map(String::as_str), Some("running"));
    assert_eq!(told_done(&home), told_before + 1, "its node is told once");
    drop(runtime);

    let reopened = DomainRuntime::open(&home.0).unwrap();
    assert_eq!(told_done(&home), told_before + 1, "opening again tells it nothing more");
    drop(reopened);
}

#[test]
fn a_dependency_that_ends_right_after_the_daemon_opens_wakes_the_saved_wait() {
    let home = Home::new();
    drop(release_closing(&home));
    let told_before = told_done(&home);

    let runtime = DomainRuntime::open(&home.0).unwrap();
    assert_eq!(home.row("pebble"), "waiting_dependency");
    release_ends(&runtime);

    assert_eq!(home.row("pebble"), "running");
    assert_eq!(told_done(&home), told_before + 1);
    drop(runtime);
}
